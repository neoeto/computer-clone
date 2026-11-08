use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio_rustls::{server::TlsStream, TlsAcceptor};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::data;
use crate::discovery::Advertiser;
use crate::engine::{Engine, PendingApproval};
use crate::error::Result;
use crate::pairing::sas_code;
use crate::protocol::{read_msg, write_msg, Request, Response};
use crate::types::CoreEvent;
use crate::util::now_ms;
use crate::{APPROVAL_TIMEOUT_SECS, PROTOCOL_VERSION};

pub struct ServiceHandle {
    pub port: u16,
    pub cancel: CancellationToken,
    pub task: JoinHandle<()>,
    /// 持有它以维持 mDNS 广播；Drop 时自动注销。
    pub advertiser: Option<Advertiser>,
}

/// 启动 TLS 监听。`listener` 已绑定，便于测试传入随机端口。
pub async fn spawn(
    engine: Arc<Engine>,
    listener: TcpListener,
    port: u16,
    advertiser: Option<Advertiser>,
) -> Result<ServiceHandle> {
    let tls = Arc::new(server_config(&engine)?);
    let cancel = CancellationToken::new();
    let task = tokio::spawn(accept_loop(engine, listener, tls, port, cancel.clone()));
    Ok(ServiceHandle {
        port,
        cancel,
        task,
        advertiser,
    })
}

fn server_config(engine: &Engine) -> Result<rustls::ServerConfig> {
    let certs = vec![CertificateDer::from(engine.identity.cert_der.clone())];
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(engine.identity.key_der.clone()));
    let config = rustls::ServerConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
        .with_no_client_auth()
        .with_single_cert(certs, key)?;
    Ok(config)
}

async fn accept_loop(
    engine: Arc<Engine>,
    listener: TcpListener,
    tls: Arc<rustls::ServerConfig>,
    port: u16,
    cancel: CancellationToken,
) {
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            accepted = listener.accept() => {
                let (stream, addr) = match accepted {
                    Ok(value) => value,
                    Err(err) => {
                        tracing::warn!(?err, "accept 失败");
                        continue;
                    }
                };
                let acceptor = TlsAcceptor::from(tls.clone());
                let engine = engine.clone();
                let token = cancel.clone();
                tokio::spawn(async move {
                    let tls_stream = match tokio::time::timeout(
                        Duration::from_secs(10),
                        acceptor.accept(stream),
                    ).await {
                        Ok(Ok(stream)) => stream,
                        Ok(Err(err)) => {
                            tracing::debug!(?err, %addr, "TLS 握手失败");
                            return;
                        }
                        Err(_) => {
                            tracing::debug!(%addr, "TLS 握手超时");
                            return;
                        }
                    };
                    if let Err(err) = handle_connection(engine, tls_stream, addr, port, token).await {
                        tracing::debug!(?err, %addr, "连接结束");
                    }
                });
            }
        }
    }
    tracing::debug!("监听循环已退出");
}

async fn handle_connection(
    engine: Arc<Engine>,
    mut tls: TlsStream<tokio::net::TcpStream>,
    addr: SocketAddr,
    port: u16,
    cancel: CancellationToken,
) -> Result<()> {
    let hello: Request = read_msg(&mut tls).await?;
    if let Request::DataHello { token } = hello {
        return handle_data_connection(engine, tls, addr, token).await;
    }
    let (protocol, device) = match hello {
        Request::Hello { protocol, device } => (protocol, device),
        _ => {
            let _ = write_msg(&mut tls, &Response::error("unexpected", "需要先发送 hello")).await;
            return Ok(());
        }
    };
    if protocol != PROTOCOL_VERSION {
        let _ = write_msg(
            &mut tls,
            &Response::error("protocol_mismatch", "两端版本不一致，请升级应用"),
        )
        .await;
        return Ok(());
    }

    write_msg(
        &mut tls,
        &Response::Hello {
            protocol: PROTOCOL_VERSION,
            device: engine.device_info(),
            fingerprint: engine.identity.fingerprint.clone(),
            port,
        },
    )
    .await?;

    let auth: Request = read_msg(&mut tls).await?;
    let code = match auth {
        Request::Auth { code } => code,
        _ => {
            let _ = write_msg(&mut tls, &Response::error("unexpected", "需要先完成认证")).await;
            return Ok(());
        }
    };

    if !engine.try_authenticate(&code).await {
        let _ = write_msg(
            &mut tls,
            &Response::error("auth_failed", "连接码不正确或已过期"),
        )
        .await;
        return Ok(());
    }

    let request_id = Uuid::new_v4().to_string();
    let sas = sas_code();
    let (tx, rx) = oneshot::channel();
    engine
        .add_pending(
            PendingApproval {
                request_id: request_id.clone(),
                device: device.clone(),
                address: addr.to_string(),
                sas: sas.clone(),
                created_at: now_ms(),
            },
            tx,
        )
        .await;

    write_msg(
        &mut tls,
        &Response::AuthPending {
            request_id: request_id.clone(),
            sas: sas.clone(),
        },
    )
    .await?;

    let _ = engine.events.send(CoreEvent::ConnectionRequest {
        request_id: request_id.clone(),
        device: device.clone(),
        address: addr.to_string(),
        sas,
    });

    let approved = tokio::select! {
        _ = cancel.cancelled() => false,
        result = tokio::time::timeout(Duration::from_secs(APPROVAL_TIMEOUT_SECS), rx) => {
            matches!(result, Ok(Ok(true)))
        }
    };
    engine.remove_pending(&request_id).await;

    if !approved {
        let _ = write_msg(&mut tls, &Response::error("denied", "源端未允许这次连接")).await;
        return Ok(());
    }

    let session = Uuid::new_v4().to_string();
    write_msg(
        &mut tls,
        &Response::AuthOk {
            session: session.clone(),
        },
    )
    .await?;

    let source_session = engine
        .register_source_session(session, device.clone(), addr.ip())
        .await;
    engine.add_client(device.clone(), addr.to_string()).await;
    let _ = engine.events.send(CoreEvent::ClientConnected {
        device: device.clone(),
        address: addr.to_string(),
    });

    let result = serve(&engine, &mut tls, &cancel, &source_session, addr.ip()).await;

    engine.remove_source_session(&source_session.id).await;
    engine.remove_client(&device.id).await;
    let _ = engine.events.send(CoreEvent::ClientDisconnected {
        device,
        address: addr.to_string(),
    });
    result
}

async fn handle_data_connection(
    engine: Arc<Engine>,
    mut tls: TlsStream<tokio::net::TcpStream>,
    addr: SocketAddr,
    token: String,
) -> Result<()> {
    let session = match engine.consume_data_ticket(&token, addr.ip()).await {
        Ok(session) => session,
        Err(err) => {
            let _ = write_msg(
                &mut tls,
                &Response::error(err.code(), format!("数据连接认证失败：{err}")),
            )
            .await;
            return Ok(());
        }
    };
    write_msg(&mut tls, &Response::DataReady).await?;
    if let Err(err) = data::serve(engine, tls, session).await {
        tracing::debug!(?err, %addr, "数据连接结束");
    }
    Ok(())
}

async fn serve(
    engine: &Arc<Engine>,
    tls: &mut TlsStream<tokio::net::TcpStream>,
    cancel: &CancellationToken,
    session: &crate::engine::SourceSession,
    peer_ip: IpAddr,
) -> Result<()> {
    loop {
        let request: Request = tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            result = read_msg(tls) => match result {
                Ok(request) => request,
                Err(_) => return Ok(()),
            }
        };

        let response = match request {
            Request::ListShares => Response::Shares {
                shares: engine.remote_shares().await,
            },
            Request::Browse { share_id, path } => match engine.browse_share(&share_id, &path).await
            {
                Ok((path, entries)) => Response::Browse { path, entries },
                Err(err) => Response::error(err.code(), err.to_string()),
            },
            Request::Manifest { share_id, cursor } => {
                match engine.manifest_page(&share_id, cursor).await {
                    Ok(page) => Response::ManifestPage { page },
                    Err(err) => Response::error(err.code(), err.to_string()),
                }
            }
            Request::DataTicket => match engine.issue_data_ticket(&session.id, peer_ip).await {
                Ok((token, expires_in)) => Response::DataTicket { token, expires_in },
                Err(err) => Response::error(err.code(), err.to_string()),
            },
            Request::Ping => Response::Pong,
            Request::Hello { .. } | Request::Auth { .. } => {
                Response::error("unexpected", "已认证，无需重复握手")
            }
            Request::DataHello { .. } => Response::error("unexpected", "数据连接不能发送控制请求"),
        };

        if write_msg(tls, &response).await.is_err() {
            return Ok(());
        }
    }
}
