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
use crate::error::{HarborError, Result};
use crate::pairing;
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

async fn read_auth(tls: &mut TlsStream<tokio::net::TcpStream>) -> Result<Request> {
    tokio::time::timeout(Duration::from_secs(15), read_msg(tls))
        .await
        .map_err(|_| HarborError::Timeout)?
}

async fn handle_connection(
    engine: Arc<Engine>,
    mut tls: TlsStream<tokio::net::TcpStream>,
    addr: SocketAddr,
    port: u16,
    cancel: CancellationToken,
) -> Result<()> {
    let hello: Request = read_auth(&mut tls).await?;
    if let Request::DataHello { token } = hello {
        return handle_data_connection(engine, tls, addr, token).await;
    }
    let (protocol, device, client_pubkey) = match hello {
        Request::Hello {
            protocol,
            device,
            pubkey,
        } => (protocol, device, pubkey),
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
    let server_device = engine.device_info();
    let trusted = engine.trust.is_trusted(&device.id, &client_pubkey);
    write_msg(
        &mut tls,
        &Response::Hello {
            protocol: PROTOCOL_VERSION,
            device: server_device.clone(),
            fingerprint: engine.identity.fingerprint.clone(),
            port,
            trusted,
            pubkey: engine.identity.public_key_hex(),
        },
    )
    .await?;

    let outcome = if trusted {
        // 已信任设备：挑战同时由源端签名，目的端可用已保存的公钥验证源端。
        let challenge = pairing::random_challenge();
        let exporter = export_auth_key(&tls, &device.id, &server_device.id)?;
        let signature = engine.identity.sign(&pairing::server_auth_message(
            &challenge,
            &device.id,
            &server_device.id,
            &exporter,
        ));
        write_msg(
            &mut tls,
            &Response::AuthChallenge {
                challenge: challenge.clone(),
                signature,
            },
        )
        .await?;
        let next: Request = read_auth(&mut tls).await?;
        match next {
            Request::AuthResponse { signature } => {
                let message =
                    pairing::auth_message(&challenge, &device.id, &server_device.id, &exporter);
                if !crate::identity::verify_signature(&client_pubkey, &message, &signature) {
                    engine.audit.record(
                        "auth_failed",
                        "warn",
                        Some(&device),
                        Some(&addr.to_string()),
                        "已信任设备签名校验失败",
                    );
                    let _ = write_msg(
                        &mut tls,
                        &Response::error("auth_failed", "设备密钥校验失败，请重新用连接码配对"),
                    )
                    .await;
                    AuthOutcome::Rejected
                } else {
                    engine.trust.touch(&device.id);
                    engine.audit.record(
                        "trusted_login",
                        "ok",
                        Some(&device),
                        Some(&addr.to_string()),
                        "已信任设备签名挑战通过",
                    );
                    AuthOutcome::Approved
                }
            }
            // 目的端本机撤销了信任：允许用连接码重新配对。
            Request::PairBegin { message } => {
                run_pairing_flow(
                    &engine,
                    &mut tls,
                    &device,
                    &client_pubkey,
                    addr,
                    &cancel,
                    Some(message),
                )
                .await?
            }
            _ => {
                let _ = write_msg(&mut tls, &Response::error("unexpected", "需要签名挑战")).await;
                AuthOutcome::Rejected
            }
        }
    } else {
        run_pairing_flow(
            &engine,
            &mut tls,
            &device,
            &client_pubkey,
            addr,
            &cancel,
            None,
        )
        .await?
    };

    match outcome {
        AuthOutcome::Approved => {}
        AuthOutcome::Denied => {
            let _ = write_msg(&mut tls, &Response::error("denied", "源端未允许这次连接")).await;
            return Ok(());
        }
        AuthOutcome::Rejected => return Ok(()),
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
    engine.audit.record(
        "connected",
        "ok",
        Some(&device),
        Some(&addr.to_string()),
        "控制会话已建立",
    );
    let _ = engine.events.send(CoreEvent::ClientConnected {
        device: device.clone(),
        address: addr.to_string(),
    });

    let result = serve(&engine, &mut tls, &cancel, &source_session, addr.ip()).await;

    engine.remove_source_session(&source_session.id).await;
    engine.remove_client(&device.id).await;
    engine.audit.record(
        "disconnected",
        "info",
        Some(&device),
        Some(&addr.to_string()),
        "控制会话已断开",
    );
    let _ = engine.events.send(CoreEvent::ClientDisconnected {
        device,
        address: addr.to_string(),
    });
    result
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AuthOutcome {
    Approved,
    Denied,
    Rejected,
}

fn export_auth_key(
    tls: &TlsStream<tokio::net::TcpStream>,
    client_id: &str,
    server_id: &str,
) -> Result<[u8; 32]> {
    let context = pairing::auth_context(client_id, server_id);
    let mut exporter = [0u8; 32];
    tls.get_ref()
        .1
        .export_keying_material(&mut exporter, b"harbor-auth-v1", Some(&context))
        .map_err(HarborError::from)?;
    Ok(exporter)
}

async fn run_pairing_flow(
    engine: &Arc<Engine>,
    tls: &mut TlsStream<tokio::net::TcpStream>,
    device: &crate::types::DeviceInfo,
    client_pubkey: &str,
    addr: SocketAddr,
    cancel: &CancellationToken,
    first_message: Option<String>,
) -> Result<AuthOutcome> {
    let Some(secret) = engine.pairing_secret().await else {
        let _ = write_msg(
            tls,
            &Response::error("auth_failed", "连接码已过期或尝试次数过多，请轮换连接码"),
        )
        .await;
        return Ok(AuthOutcome::Rejected);
    };
    let message_a = match first_message {
        Some(message) => message,
        None => {
            let begin: Request = read_auth(tls).await?;
            match begin {
                Request::PairBegin { message } => message,
                _ => {
                    let _ = write_msg(tls, &Response::error("unexpected", "需要开始配对")).await;
                    return Ok(AuthOutcome::Rejected);
                }
            }
        }
    };
    let server_id = engine.device_info().id;
    let (state, message_b) = match pairing::begin_server(&secret, &device.id, &server_id) {
        Ok(value) => value,
        Err(err) => {
            let _ = write_msg(tls, &Response::error(err.code(), err.to_string())).await;
            return Ok(AuthOutcome::Rejected);
        }
    };
    write_msg(tls, &Response::PairChallenge { message: message_b }).await?;

    let finish: Request = read_auth(tls).await?;
    let confirm = match finish {
        Request::PairFinish { confirm } => confirm,
        _ => {
            let _ = write_msg(tls, &Response::error("unexpected", "需要完成配对")).await;
            return Ok(AuthOutcome::Rejected);
        }
    };
    let material = match state.finish(&message_a) {
        Ok(material) => material,
        Err(_) => {
            engine.record_pair_failure().await;
            engine.audit.record(
                "auth_failed",
                "warn",
                Some(device),
                Some(&addr.to_string()),
                "SPAKE2 握手失败",
            );
            let _ = write_msg(tls, &Response::error("auth_failed", "连接码不正确或已过期")).await;
            return Ok(AuthOutcome::Rejected);
        }
    };
    if !pairing::secure_eq(&confirm, &material.confirm_client()) {
        engine.record_pair_failure().await;
        engine.audit.record(
            "auth_failed",
            "warn",
            Some(device),
            Some(&addr.to_string()),
            "SPAKE2 确认失败（连接码错误或过期）",
        );
        let _ = write_msg(tls, &Response::error("auth_failed", "连接码不正确或已过期")).await;
        return Ok(AuthOutcome::Rejected);
    }

    engine.consume_pairing().await;
    let request_id = Uuid::new_v4().to_string();
    let sas = material.sas();
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
        tls,
        &Response::PairFinishAck {
            confirm: material.confirm_server(),
            request_id: request_id.clone(),
            sas: sas.clone(),
        },
    )
    .await?;
    engine.audit.record(
        "connection_request",
        "warn",
        Some(device),
        Some(&addr.to_string()),
        "等待源端审批",
    );
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
    engine.audit.record(
        if approved { "approved" } else { "denied" },
        if approved { "ok" } else { "warn" },
        Some(device),
        Some(&addr.to_string()),
        if approved {
            "源端允许连接并写入信任设备"
        } else {
            "源端拒绝连接"
        },
    );
    if approved {
        if let Err(err) = engine.trust.trust(device, client_pubkey) {
            tracing::warn!(?err, "写入信任设备失败");
        }
        Ok(AuthOutcome::Approved)
    } else {
        Ok(AuthOutcome::Denied)
    }
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
            Request::Hello { .. }
            | Request::PairBegin { .. }
            | Request::PairFinish { .. }
            | Request::AuthResponse { .. } => Response::error("unexpected", "已认证，无需重复握手"),
            Request::DataHello { .. } => Response::error("unexpected", "数据连接不能发送控制请求"),
        };

        if write_msg(tls, &response).await.is_err() {
            return Ok(());
        }
    }
}
