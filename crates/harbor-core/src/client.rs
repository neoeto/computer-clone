use std::sync::Arc;
use std::time::Duration;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio::time::timeout;
use tokio_rustls::client::TlsStream;
use tokio_rustls::TlsConnector;
use tokio_util::sync::CancellationToken;

use crate::engine::{Engine, RemoteSession};
use crate::error::{HarborError, Result};
use crate::protocol::{read_msg, write_msg, Request, Response};
use crate::types::{short_fingerprint, CoreEvent, RemoteSourceInfo};
use crate::util::now_ms;
use crate::PROTOCOL_VERSION;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const APPROVAL_TIMEOUT: Duration = Duration::from_secs(70);

/// 目的端发起连接。结果通过 `CoreEvent` 通知 UI，不阻塞命令。
pub async fn connect(
    engine: Arc<Engine>,
    connect_id: String,
    host: String,
    port: u16,
    code: String,
    expected_fingerprint: Option<String>,
    cancel: CancellationToken,
) {
    match connect_inner(
        &engine,
        &connect_id,
        &host,
        port,
        &code,
        expected_fingerprint.as_deref(),
        &cancel,
    )
    .await
    {
        Ok((session_id, stream, source, host, port)) => {
            let session = Arc::new(RemoteSession {
                info: source.clone(),
                host,
                port,
                conn: Mutex::new(stream),
            });
            engine.insert_session(session_id.clone(), session).await;
            let _ = engine.events.send(CoreEvent::ConnectSucceeded {
                connect_id,
                session_id,
                source,
            });
        }
        Err(err) => {
            tracing::info!(?err, "连接失败");
            let _ = engine.events.send(CoreEvent::ConnectFailed {
                connect_id,
                message: err.to_string(),
            });
        }
    }
}

async fn connect_inner(
    engine: &Arc<Engine>,
    connect_id: &str,
    host: &str,
    port: u16,
    code: &str,
    expected_fingerprint: Option<&str>,
    cancel: &CancellationToken,
) -> Result<(String, TlsStream<TcpStream>, RemoteSourceInfo, String, u16)> {
    let address = format!("{host}:{port}");
    let mut tls = open_tls(host, port, expected_fingerprint).await?;
    let fingerprint = peer_fingerprint(&tls)?;
    if let Some(expected) = expected_fingerprint {
        let expected = expected.trim().to_uppercase();
        if !expected.is_empty()
            && !fingerprint.starts_with(&expected)
            && !short_fingerprint(&fingerprint)
                .to_uppercase()
                .starts_with(&expected)
        {
            return Err(HarborError::Tls(
                "证书指纹与发现记录不一致，连接已中止".into(),
            ));
        }
    }

    write_msg(
        &mut tls,
        &Request::Hello {
            protocol: PROTOCOL_VERSION,
            device: engine.device_info(),
        },
    )
    .await?;

    let hello: Response = timeout(HANDSHAKE_TIMEOUT, read_msg(&mut tls))
        .await
        .map_err(|_| HarborError::Timeout)??;
    let remote_device = match hello {
        Response::Hello {
            protocol,
            device,
            fingerprint: server_fp,
            ..
        } => {
            if protocol != PROTOCOL_VERSION {
                return Err(HarborError::Protocol("两端协议版本不一致".into()));
            }
            if !server_fp.is_empty() && server_fp != fingerprint {
                return Err(HarborError::Tls("服务端证书指纹不一致".into()));
            }
            device
        }
        Response::Error { code, message } => {
            return Err(map_error(&code, &message));
        }
        _ => return Err(HarborError::Protocol("握手响应异常".into())),
    };

    write_msg(
        &mut tls,
        &Request::Auth {
            code: code.to_string(),
        },
    )
    .await?;

    let first: Response = timeout(HANDSHAKE_TIMEOUT, read_msg(&mut tls))
        .await
        .map_err(|_| HarborError::Timeout)??;
    let sas = match first {
        Response::AuthPending { sas, .. } => sas,
        Response::Error { code, message } => return Err(map_error(&code, &message)),
        _ => return Err(HarborError::Protocol("认证响应异常".into())),
    };

    let source = RemoteSourceInfo {
        device: remote_device,
        address,
        port,
        fingerprint: fingerprint.clone(),
        connected_at: now_ms(),
    };
    let _ = engine.events.send(CoreEvent::ConnectPending {
        connect_id: connect_id.to_string(),
        source: source.clone(),
        sas,
    });

    let second: Response = tokio::select! {
        _ = cancel.cancelled() => return Err(HarborError::Other("已取消连接".into())),
        result = timeout(APPROVAL_TIMEOUT, read_msg(&mut tls)) => {
            result.map_err(|_| HarborError::ApprovalTimeout)??
        }
    };
    let session_id = match second {
        Response::AuthOk { session } => session,
        Response::Error { code, message } => return Err(map_error(&code, &message)),
        _ => return Err(HarborError::Protocol("确认响应异常".into())),
    };

    Ok((session_id, tls, source, host.to_string(), port))
}

/// 建立 TLS 连接并完成证书指纹比对。控制连接与数据连接共用。
pub(crate) async fn open_tls(
    host: &str,
    port: u16,
    expected_fingerprint: Option<&str>,
) -> Result<TlsStream<TcpStream>> {
    let tcp = timeout(CONNECT_TIMEOUT, TcpStream::connect((host, port)))
        .await
        .map_err(|_| HarborError::Timeout)??;

    let connector = TlsConnector::from(client_config());
    let server_name = ServerName::try_from("harbor.local")
        .map_err(|_| HarborError::Protocol("SNI 不合法".into()))?;
    let tls = timeout(HANDSHAKE_TIMEOUT, connector.connect(server_name, tcp))
        .await
        .map_err(|_| HarborError::Timeout)??;

    let fingerprint = peer_fingerprint(&tls)?;
    if let Some(expected) = expected_fingerprint {
        let expected = expected.trim().to_uppercase();
        if !expected.is_empty()
            && !fingerprint.starts_with(&expected)
            && !short_fingerprint(&fingerprint)
                .to_uppercase()
                .starts_with(&expected)
        {
            return Err(HarborError::Tls(
                "证书指纹与发现记录不一致，连接已中止".into(),
            ));
        }
    }
    Ok(tls)
}

fn peer_fingerprint(tls: &TlsStream<TcpStream>) -> Result<String> {
    let peer_cert = tls
        .get_ref()
        .1
        .peer_certificates()
        .and_then(|certs| certs.first())
        .ok_or_else(|| HarborError::Tls("对方没有提供证书".into()))?;
    Ok(crate::identity::fingerprint(peer_cert.as_ref()))
}

fn map_error(code: &str, message: &str) -> HarborError {
    match code {
        "auth_failed" => HarborError::AuthFailed,
        "denied" => HarborError::Denied,
        "approval_timeout" => HarborError::ApprovalTimeout,
        "protocol_mismatch" => HarborError::Protocol(message.to_string()),
        _ => HarborError::Other(message.to_string()),
    }
}

fn client_config() -> Arc<rustls::ClientConfig> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verifier = Arc::new(AcceptAnyCert(provider));
    Arc::new(
        rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(verifier)
            .with_no_client_auth(),
    )
}

/// 自签证书场景：接受任意证书，但连接层会自行比对指纹。
/// 真正的密码学身份校验（设备公钥 + 指纹固定）在 M3 完成。
struct AcceptAnyCert(Arc<CryptoProvider>);

impl std::fmt::Debug for AcceptAnyCert {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AcceptAnyCert")
    }
}

impl ServerCertVerifier for AcceptAnyCert {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}
