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
use crate::pairing;
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
            engine.audit.record(
                "connected_out",
                "ok",
                Some(&source.device),
                Some(&source.address),
                "已连接到源端",
            );
            let _ = engine.events.send(CoreEvent::ConnectSucceeded {
                connect_id,
                session_id,
                source,
            });
        }
        Err(err) => {
            tracing::info!(?err, "连接失败");
            engine.audit.record(
                "connect_failed",
                "warn",
                None,
                Some(&format!("{host}:{port}")),
                err.to_string(),
            );
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
    let local_device = engine.device_info();
    write_msg(
        &mut tls,
        &Request::Hello {
            protocol: PROTOCOL_VERSION,
            device: local_device.clone(),
            pubkey: engine.identity.public_key_hex(),
        },
    )
    .await?;

    let hello: Response = timeout(HANDSHAKE_TIMEOUT, read_msg(&mut tls))
        .await
        .map_err(|_| HarborError::Timeout)??;
    let (remote_device, trusted, server_pubkey) = match hello {
        Response::Hello {
            protocol,
            device,
            fingerprint: server_fp,
            trusted,
            pubkey,
            ..
        } => {
            if protocol != PROTOCOL_VERSION {
                return Err(HarborError::Protocol("两端协议版本不一致".into()));
            }
            if !server_fp.is_empty() && server_fp != fingerprint {
                return Err(HarborError::Tls("服务端证书指纹不一致".into()));
            }
            (device, trusted, pubkey)
        }
        Response::Error { code, message } => {
            return Err(map_error(&code, &message));
        }
        _ => return Err(HarborError::Protocol("握手响应异常".into())),
    };
    let source = RemoteSourceInfo {
        device: remote_device.clone(),
        address: address.clone(),
        port,
        fingerprint: fingerprint.clone(),
        connected_at: now_ms(),
    };

    let local_trust = engine.trust.find(&remote_device.id);
    let local_trusted = local_trust
        .as_ref()
        .map(|record| {
            !record.revoked
                && !server_pubkey.is_empty()
                && record.pubkey.eq_ignore_ascii_case(&server_pubkey)
        })
        .unwrap_or(false);

    let mut paired_with_code = false;
    let session_id = if trusted && local_trusted {
        // 已信任设备：客户端验证源端签名，再对当前 TLS exporter 签名。
        let first: Response = timeout(HANDSHAKE_TIMEOUT, read_msg(&mut tls))
            .await
            .map_err(|_| HarborError::Timeout)??;
        let (challenge, server_signature) = match first {
            Response::AuthChallenge {
                challenge,
                signature,
            } => (challenge, signature),
            Response::Error { code, message } => return Err(map_error(&code, &message)),
            _ => return Err(HarborError::Protocol("挑战响应异常".into())),
        };
        let context = pairing::auth_context(&local_device.id, &remote_device.id);
        let mut exporter = [0u8; 32];
        tls.get_ref()
            .1
            .export_keying_material(&mut exporter, b"harbor-auth-v1", Some(&context))
            .map_err(HarborError::from)?;
        let server_message = pairing::server_auth_message(
            &challenge,
            &local_device.id,
            &remote_device.id,
            &exporter,
        );
        if !crate::identity::verify_signature(&server_pubkey, &server_message, &server_signature) {
            return Err(HarborError::Tls("源端设备签名校验失败".into()));
        }
        let message =
            pairing::auth_message(&challenge, &local_device.id, &remote_device.id, &exporter);
        write_msg(
            &mut tls,
            &Request::AuthResponse {
                signature: engine.identity.sign(&message),
            },
        )
        .await?;
        let reply: Response = timeout(HANDSHAKE_TIMEOUT, read_msg(&mut tls))
            .await
            .map_err(|_| HarborError::Timeout)??;
        match reply {
            Response::AuthOk { session } => session,
            Response::Error { code, message } => return Err(map_error(&code, &message)),
            _ => return Err(HarborError::Protocol("认证响应异常".into())),
        }
    } else {
        // 首次配对，或本机撤销/丢失了对源端的信任：走 SPAKE2。
        if trusted {
            // 源端已发送签名挑战；本机决定改用连接码时先消费掉它。
            let first: Response = timeout(HANDSHAKE_TIMEOUT, read_msg(&mut tls))
                .await
                .map_err(|_| HarborError::Timeout)??;
            match first {
                Response::AuthChallenge { .. } => {}
                Response::Error { code, message } => return Err(map_error(&code, &message)),
                _ => return Err(HarborError::Protocol("挑战响应异常".into())),
            }
        }
        let secret: String = code.chars().filter(|ch| ch.is_ascii_digit()).collect();
        if secret.len() != 6 {
            if trusted {
                return Err(HarborError::Other(
                    "本机尚未信任该源端，请输入连接码重新配对".into(),
                ));
            }
            return Err(HarborError::Other(
                "该设备尚未信任，请向源端获取 6 位连接码".into(),
            ));
        }
        paired_with_code = true;
        pair_with_code(
            engine,
            &mut tls,
            connect_id,
            cancel,
            &local_device,
            &remote_device,
            &source,
            &secret,
        )
        .await?
    };

    // 首次配对成功后，把源端也写入本机信任库；之后重连可双向验证。
    if paired_with_code {
        if let Err(err) = engine.trust.trust(&source.device, &server_pubkey) {
            tracing::warn!(?err, "保存源端信任失败");
        }
    } else {
        engine.trust.touch(&source.device.id);
    }

    Ok((session_id, tls, source, host.to_string(), port))
}

#[allow(clippy::too_many_arguments)]
async fn pair_with_code(
    engine: &Arc<Engine>,
    tls: &mut TlsStream<TcpStream>,
    connect_id: &str,
    cancel: &CancellationToken,
    local_device: &crate::types::DeviceInfo,
    remote_device: &crate::types::DeviceInfo,
    source: &RemoteSourceInfo,
    secret: &str,
) -> Result<String> {
    if cancel.is_cancelled() {
        return Err(HarborError::Other("已取消连接".into()));
    }
    let (state, message_a) = pairing::begin_client(secret, &local_device.id, &remote_device.id)?;
    write_msg(tls, &Request::PairBegin { message: message_a }).await?;
    let challenge: Response = timeout(HANDSHAKE_TIMEOUT, read_msg(tls))
        .await
        .map_err(|_| HarborError::Timeout)??;
    let message_b = match challenge {
        Response::PairChallenge { message } => message,
        Response::Error { code, message } => return Err(map_error(&code, &message)),
        _ => return Err(HarborError::Protocol("配对响应异常".into())),
    };
    let material = state.finish(&message_b)?;

    write_msg(
        tls,
        &Request::PairFinish {
            confirm: material.confirm_client(),
        },
    )
    .await?;
    let ack: Response = timeout(HANDSHAKE_TIMEOUT, read_msg(tls))
        .await
        .map_err(|_| HarborError::Timeout)??;
    let sas = match ack {
        Response::PairFinishAck { confirm, sas, .. } => {
            if !pairing::secure_eq(&confirm, &material.confirm_server())
                || !pairing::secure_eq(&sas, &material.sas())
            {
                return Err(HarborError::AuthFailed);
            }
            sas
        }
        Response::Error { code, message } => return Err(map_error(&code, &message)),
        _ => return Err(HarborError::Protocol("配对确认异常".into())),
    };

    let _ = engine.events.send(CoreEvent::ConnectPending {
        connect_id: connect_id.to_string(),
        source: source.clone(),
        sas,
    });

    let second: Response = tokio::select! {
        _ = cancel.cancelled() => return Err(HarborError::Other("已取消连接".into())),
        result = timeout(APPROVAL_TIMEOUT, read_msg(tls)) => {
            result.map_err(|_| HarborError::ApprovalTimeout)??
        }
    };
    match second {
        Response::AuthOk { session } => Ok(session),
        Response::Error { code, message } => Err(map_error(&code, &message)),
        _ => Err(HarborError::Protocol("确认响应异常".into())),
    }
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
