use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use harbor_core::error::HarborError;
use harbor_core::types::CoreEvent;
use harbor_core::Engine;
use tokio::sync::broadcast;
use tokio::time::timeout;

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("harbor-e2e-{tag}-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&dir).unwrap();
    dir
}

async fn wait_for<F>(rx: &mut broadcast::Receiver<CoreEvent>, mut predicate: F) -> CoreEvent
where
    F: FnMut(&CoreEvent) -> bool,
{
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            panic!("等待事件超时");
        }
        match timeout(remaining, rx.recv()).await {
            Ok(Ok(event)) => {
                if predicate(&event) {
                    return event;
                }
            }
            Ok(Err(err)) => panic!("事件通道错误：{err}"),
            Err(_) => panic!("等待事件超时"),
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn full_pairing_and_browse_flow() {
    // 测试环境关闭 mDNS，避免占用 5353 与系统权限。
    std::env::set_var("HARBOR_DISABLE_MDNS", "1");

    let root = temp_dir("root");
    let shared = root.join("shared");
    fs::create_dir_all(shared.join("sub")).unwrap();
    fs::write(shared.join("hello.txt"), b"hello harbor").unwrap();
    fs::write(shared.join("sub").join("world.txt"), b"world").unwrap();

    let source = Engine::new(root.join("source-data")).unwrap();
    {
        let mut cfg = source.config.write().await;
        cfg.port = 0;
    }
    let share = source.add_share(shared.to_str().unwrap()).await.unwrap();
    assert_eq!(share.file_count, 2);
    assert_eq!(share.total_bytes, 17);

    let info = source.start_source().await.unwrap();
    assert!(info.running);
    assert_eq!(info.shares.len(), 1);
    assert_eq!(info.code.len(), 6);
    assert!(!info.addresses.is_empty());

    let dest = Engine::new(root.join("dest-data")).unwrap();
    let mut dest_events = dest.subscribe();
    let mut source_events = source.subscribe();

    // ---- 正常配对 ----
    let code = info.code.clone();
    dest.start_connect("127.0.0.1".into(), info.port, code.clone(), None)
        .await;

    let pending = wait_for(&mut dest_events, |e| {
        matches!(e, CoreEvent::ConnectPending { .. })
    })
    .await;
    let (pending_id, sas_dest, source_info) = match pending {
        CoreEvent::ConnectPending {
            connect_id,
            source,
            sas,
        } => (connect_id, sas, source),
        _ => unreachable!(),
    };
    assert_eq!(source_info.port, info.port);

    let request = wait_for(&mut source_events, |e| {
        matches!(e, CoreEvent::ConnectionRequest { .. })
    })
    .await;
    let (request_id, sas_source) = match request {
        CoreEvent::ConnectionRequest {
            request_id, sas, ..
        } => (request_id, sas),
        _ => unreachable!(),
    };
    assert_eq!(sas_dest, sas_source, "双端 SAS 必须一致");

    source.approve(&request_id, true).await.unwrap();

    let succeeded = wait_for(&mut dest_events, |e| {
        matches!(e, CoreEvent::ConnectSucceeded { .. })
    })
    .await;
    let (succeeded_id, session_id) = match succeeded {
        CoreEvent::ConnectSucceeded {
            connect_id,
            session_id,
            ..
        } => (connect_id, session_id),
        _ => unreachable!(),
    };
    assert_eq!(pending_id, succeeded_id);

    // ---- 共享清单与浏览 ----
    let shares = dest.remote_list_shares(&session_id).await.unwrap();
    assert_eq!(shares.len(), 1);
    assert_eq!(shares[0].name, "shared");
    assert_eq!(shares[0].file_count, 2);

    let (path, entries) = dest
        .remote_browse(&session_id, &shares[0].id, "")
        .await
        .unwrap();
    assert_eq!(path, "");
    assert!(entries.iter().any(|e| e.name == "hello.txt" && !e.is_dir));
    assert!(entries.iter().any(|e| e.name == "sub" && e.is_dir));
    assert!(
        entries.iter().position(|e| e.is_dir).unwrap()
            < entries.iter().position(|e| !e.is_dir).unwrap(),
        "目录应排在文件前"
    );

    let (_, sub_entries) = dest
        .remote_browse(&session_id, &shares[0].id, "sub")
        .await
        .unwrap();
    assert_eq!(sub_entries.len(), 1);
    assert_eq!(sub_entries[0].name, "world.txt");

    // ---- 路径穿越必须被拒绝 ----
    for bad in ["../", "../../etc", "sub/../../secret", "/etc/passwd"] {
        let err = dest
            .remote_browse(&session_id, &shares[0].id, bad)
            .await
            .unwrap_err();
        assert!(
            matches!(err, HarborError::InvalidPath),
            "路径 {bad} 应被拒绝，实际：{err:?}"
        );
    }

    // ---- 暂停共享后不可见 ----
    source.set_share_enabled(&share.id, false).await.unwrap();
    let hidden = dest.remote_list_shares(&session_id).await.unwrap();
    assert!(hidden.is_empty());
    source.set_share_enabled(&share.id, true).await.unwrap();

    dest.disconnect_remote(&session_id).await;

    // ---- 已信任设备重连：无需连接码，也无需再次审批 ----
    let trusted = source.trusted_devices();
    assert_eq!(trusted.len(), 1);
    assert!(!trusted[0].revoked);
    assert!(trusted[0].pubkey.len() == 64);

    let mut dest_events = dest.subscribe();
    dest.start_connect("127.0.0.1".into(), info.port, "000000".into(), None)
        .await;
    let trusted_ok = wait_for(&mut dest_events, |event| {
        matches!(event, CoreEvent::ConnectSucceeded { .. })
    })
    .await;
    let trusted_session = match trusted_ok {
        CoreEvent::ConnectSucceeded { session_id, .. } => session_id,
        _ => unreachable!(),
    };
    let shares = dest.remote_list_shares(&trusted_session).await.unwrap();
    assert_eq!(shares.len(), 1);
    dest.disconnect_remote(&trusted_session).await;

    // ---- 本机撤销对源端的信任：源端仍信任本机，但本机用连接码回退 ----
    let source_device_id = source.device_info().id.clone();
    dest.revoke_trusted_device(&source_device_id).unwrap();
    assert!(dest
        .trusted_devices()
        .iter()
        .any(|device| device.id == source_device_id && device.revoked));
    let fallback_code = source.source_info().await.code;
    let mut fallback_dest_events = dest.subscribe();
    let mut fallback_source_events = source.subscribe();
    dest.start_connect("127.0.0.1".into(), info.port, fallback_code, None)
        .await;
    let _ = wait_for(&mut fallback_dest_events, |event| {
        matches!(event, CoreEvent::ConnectPending { .. })
    })
    .await;
    let request = wait_for(&mut fallback_source_events, |event| {
        matches!(event, CoreEvent::ConnectionRequest { .. })
    })
    .await;
    let request_id = match request {
        CoreEvent::ConnectionRequest { request_id, .. } => request_id,
        _ => unreachable!(),
    };
    source.approve(&request_id, true).await.unwrap();
    let fallback_ok = wait_for(&mut fallback_dest_events, |event| {
        matches!(event, CoreEvent::ConnectSucceeded { .. })
    })
    .await;
    let fallback_session = match fallback_ok {
        CoreEvent::ConnectSucceeded { session_id, .. } => session_id,
        _ => unreachable!(),
    };
    assert!(
        dest.trusted_devices()
            .iter()
            .any(|device| device.id == source_device_id && !device.revoked),
        "回退配对成功后应重新信任源端"
    );
    dest.disconnect_remote(&fallback_session).await;

    // 撤销信任后回到连接码流程，用于覆盖拒绝与错码。
    let dest_device_id = dest.device_info().id.clone();
    source.revoke_trusted_device(&dest_device_id).unwrap();
    assert!(source.trusted_devices()[0].revoked);

    // ---- 拒绝连接 ----
    let rotated = source.rotate_code().await;
    let mut dest_events = dest.subscribe();
    let mut reject_source_events = source.subscribe();
    dest.start_connect("127.0.0.1".into(), info.port, rotated.code.clone(), None)
        .await;
    let _pending = wait_for(&mut dest_events, |e| {
        matches!(e, CoreEvent::ConnectPending { .. })
    })
    .await;
    let request = wait_for(&mut reject_source_events, |e| {
        matches!(e, CoreEvent::ConnectionRequest { .. })
    })
    .await;
    let request_id = match request {
        CoreEvent::ConnectionRequest { request_id, .. } => request_id,
        _ => unreachable!(),
    };
    source.approve(&request_id, false).await.unwrap();
    let failed = wait_for(&mut dest_events, |e| {
        matches!(e, CoreEvent::ConnectFailed { .. })
    })
    .await;
    match failed {
        CoreEvent::ConnectFailed { message, .. } => assert!(message.contains("拒绝")),
        _ => unreachable!(),
    }

    // ---- 错误连接码 ----
    let rotated = source.rotate_code().await;
    let wrong = if rotated.code == "000000" {
        "000001".to_string()
    } else {
        "000000".to_string()
    };
    let mut dest_events = dest.subscribe();
    dest.start_connect("127.0.0.1".into(), info.port, wrong, None)
        .await;
    let failed = wait_for(&mut dest_events, |e| {
        matches!(e, CoreEvent::ConnectFailed { .. })
    })
    .await;
    match failed {
        CoreEvent::ConnectFailed { message, .. } => assert!(message.contains("连接码")),
        _ => unreachable!(),
    }

    // ---- 停止服务 ----
    source.stop_source().await;
    let stopped = source.source_info().await;
    assert!(!stopped.running);
    assert!(stopped.code.is_empty());

    source.stop_discovery().await;
    dest.stop_discovery().await;
    fs::remove_dir_all(&root).ok();
}
