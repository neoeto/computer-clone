use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use harbor_core::data::RemoteData;
use harbor_core::error::HarborError;
use harbor_core::sync::{ConflictPolicy, CreateTaskRequest, SyncMode, TaskState};
use harbor_core::types::CoreEvent;
use harbor_core::Engine;
use tokio::sync::broadcast;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("harbor-m2-{tag}-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn random_bytes(len: usize, seed: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(len);
    let mut state = seed;
    for _ in 0..len {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        out.push((state >> 33) as u8);
    }
    out
}

fn mtime_ms(path: &Path) -> u64 {
    fs::metadata(path)
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

fn set_mtime(path: &Path, time: SystemTime) {
    fs::OpenOptions::new()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(time)
        .unwrap();
}

async fn wait_for<F>(rx: &mut broadcast::Receiver<CoreEvent>, mut predicate: F) -> CoreEvent
where
    F: FnMut(&CoreEvent) -> bool,
{
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
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

async fn pair(source: &std::sync::Arc<Engine>, dest: &std::sync::Arc<Engine>, port: u16) -> String {
    let code = source.source_info().await.code;
    let mut dest_events = dest.subscribe();
    let mut source_events = source.subscribe();
    dest.start_connect("127.0.0.1".into(), port, code, None)
        .await;

    let pending = wait_for(&mut dest_events, |event| {
        matches!(event, CoreEvent::ConnectPending { .. })
    })
    .await;
    let sas_dest = match pending {
        CoreEvent::ConnectPending { sas, .. } => sas,
        _ => unreachable!(),
    };
    let request = wait_for(&mut source_events, |event| {
        matches!(event, CoreEvent::ConnectionRequest { .. })
    })
    .await;
    let (request_id, sas_source) = match request {
        CoreEvent::ConnectionRequest {
            request_id, sas, ..
        } => (request_id, sas),
        _ => unreachable!(),
    };
    assert_eq!(sas_dest, sas_source);
    source.approve(&request_id, true).await.unwrap();

    let succeeded = wait_for(&mut dest_events, |event| {
        matches!(event, CoreEvent::ConnectSucceeded { .. })
    })
    .await;
    match succeeded {
        CoreEvent::ConnectSucceeded { session_id, .. } => session_id,
        _ => unreachable!(),
    }
}

async fn run_task(
    dest: &std::sync::Arc<Engine>,
    session_id: &str,
    share_id: &str,
    dest_root: &Path,
    mode: SyncMode,
    conflict: ConflictPolicy,
) -> harbor_core::sync::TaskSummary {
    let detail = dest
        .create_sync_task(CreateTaskRequest {
            session_id: session_id.to_string(),
            share_ids: vec![share_id.to_string()],
            dest_root: dest_root.to_string_lossy().to_string(),
            mode,
            conflict,
            verify_strict: false,
        })
        .await
        .unwrap();
    let summary = dest.start_sync_task(&detail.summary.id).await.unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        let detail = dest.get_sync_task(&summary.id).await.unwrap();
        match detail.summary.state {
            TaskState::Done => return detail.summary,
            TaskState::Interrupted | TaskState::Failed | TaskState::Cancelled => {
                panic!(
                    "任务未完成：{:?} · {} · 失败 {}",
                    detail.summary.state,
                    detail.summary.message,
                    detail.failures.len()
                );
            }
            _ => {}
        }
        if tokio::time::Instant::now() > deadline {
            panic!("任务超时：{:?}", detail.summary.message);
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
}

fn find_file(root: &Path, name: &str) -> Option<PathBuf> {
    for item in walkdir::WalkDir::new(root).into_iter().flatten() {
        if item.file_type().is_file() && item.file_name().to_string_lossy() == name {
            return Some(item.path().to_path_buf());
        }
    }
    None
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn full_transfer_modes_resume_and_security() {
    std::env::set_var("HARBOR_DISABLE_MDNS", "1");

    let root = temp_dir("root");
    let shared = root.join("shared");
    fs::create_dir_all(shared.join("dir")).unwrap();
    fs::create_dir_all(shared.join("empty-dir")).unwrap();
    fs::write(shared.join("a.txt"), b"alpha").unwrap();
    fs::write(shared.join("empty.txt"), b"").unwrap();
    let large = random_bytes(3 * 1024 * 1024 + 777, 99);
    fs::write(shared.join("dir/nested.bin"), &large).unwrap();
    fs::write(shared.join(".DS_Store"), b"junk").unwrap();

    let source = Engine::new(root.join("source-data")).unwrap();
    source.config.write().await.port = 0;
    let share = source.add_share(shared.to_str().unwrap()).await.unwrap();
    let info = source.start_source().await.unwrap();

    let dest = Engine::new(root.join("dest-data")).unwrap();
    let session_id = pair(&source, &dest, info.port).await;

    // ---- 1. 增量同步：目录、空文件、多块文件、忽略规则、mtime ----
    let dest_root = root.join("dest");
    let task = run_task(
        &dest,
        &session_id,
        &share.id,
        &dest_root,
        SyncMode::Delta,
        ConflictPolicy::Overwrite,
    )
    .await;
    assert_eq!(task.failed_files, 0, "{:?}", task.message);
    let target = dest_root.join("shared");
    assert_eq!(fs::read(target.join("a.txt")).unwrap(), b"alpha");
    assert_eq!(fs::read(target.join("empty.txt")).unwrap(), b"");
    assert_eq!(fs::read(target.join("dir/nested.bin")).unwrap(), large);
    assert!(target.join("empty-dir").is_dir());
    assert!(!target.join(".DS_Store").exists());
    assert!(task.done_files >= 3);
    assert!(
        mtime_ms(&target.join("a.txt")).abs_diff(mtime_ms(&shared.join("a.txt"))) <= 2,
        "修改时间应保留"
    );
    assert!(
        !walkdir::WalkDir::new(&target)
            .into_iter()
            .flatten()
            .any(|entry| entry.path().extension().and_then(|ext| ext.to_str()) == Some("part")),
        "完成后不应残留 .part"
    );

    // ---- 2. 第二次增量：跳过未变化文件、保留本地多余文件 ----
    fs::write(target.join("local.txt"), b"local-only").unwrap();
    let future = SystemTime::now() + Duration::from_secs(5);
    fs::write(shared.join("a.txt"), b"alpha-2").unwrap();
    set_mtime(&shared.join("a.txt"), future);
    fs::write(shared.join("new.txt"), b"new-file").unwrap();
    let task = run_task(
        &dest,
        &session_id,
        &share.id,
        &dest_root,
        SyncMode::Delta,
        ConflictPolicy::Overwrite,
    )
    .await;
    assert_eq!(task.failed_files, 0);
    assert_eq!(fs::read(target.join("a.txt")).unwrap(), b"alpha-2");
    assert_eq!(fs::read(target.join("new.txt")).unwrap(), b"new-file");
    assert_eq!(fs::read(target.join("local.txt")).unwrap(), b"local-only");
    assert!(task.skipped_files >= 1);

    // ---- 3. 仅新增：绝不覆盖本地已有文件 ----
    let future2 = SystemTime::now() + Duration::from_secs(10);
    fs::write(shared.join("a.txt"), b"alpha-3").unwrap();
    set_mtime(&shared.join("a.txt"), future2);
    let task = run_task(
        &dest,
        &session_id,
        &share.id,
        &dest_root,
        SyncMode::NewOnly,
        ConflictPolicy::Overwrite,
    )
    .await;
    assert_eq!(task.failed_files, 0);
    assert_eq!(fs::read(target.join("a.txt")).unwrap(), b"alpha-2");
    assert!(task.skipped_files >= 1);

    // ---- 4. 镜像：源端删除同步删除，本地多余文件进入隔离区 ----
    fs::remove_file(shared.join("a.txt")).unwrap();
    fs::write(target.join("extra.txt"), b"extra").unwrap();
    let task = run_task(
        &dest,
        &session_id,
        &share.id,
        &dest_root,
        SyncMode::Mirror,
        ConflictPolicy::Overwrite,
    )
    .await;
    assert_eq!(task.failed_files, 0);
    assert!(!target.join("a.txt").exists());
    assert!(!target.join("extra.txt").exists());
    assert!(
        find_file(&target.join(".harbor-trash"), "a.txt").is_some(),
        "被删除的源文件应进入隔离区"
    );
    assert!(
        find_file(&target.join(".harbor-trash"), "extra.txt").is_some(),
        "本地多余文件应进入隔离区"
    );
    assert!(target.join("new.txt").exists());

    // ---- 5. 保留两者：本地文件保留，源端版本改名落盘 ----
    fs::write(shared.join("conflict.txt"), b"source-1").unwrap();
    let _ = run_task(
        &dest,
        &session_id,
        &share.id,
        &dest_root,
        SyncMode::Delta,
        ConflictPolicy::Overwrite,
    )
    .await;
    assert_eq!(fs::read(target.join("conflict.txt")).unwrap(), b"source-1");

    let future3 = SystemTime::now() + Duration::from_secs(15);
    fs::write(shared.join("conflict.txt"), b"source-2").unwrap();
    set_mtime(&shared.join("conflict.txt"), future3);
    fs::write(target.join("conflict.txt"), b"local-2").unwrap();
    let task = run_task(
        &dest,
        &session_id,
        &share.id,
        &dest_root,
        SyncMode::Delta,
        ConflictPolicy::KeepBoth,
    )
    .await;
    assert_eq!(task.failed_files, 0);
    assert_eq!(fs::read(target.join("conflict.txt")).unwrap(), b"local-2");
    let both = find_file(&target, "conflict (来自").or_else(|| {
        fs::read_dir(&target).unwrap().flatten().find_map(|item| {
            let name = item.file_name().to_string_lossy().to_string();
            if name.starts_with("conflict (来自") {
                Some(item.path())
            } else {
                None
            }
        })
    });
    let both = both.expect("应生成保留两者的副本");
    assert_eq!(fs::read(both).unwrap(), b"source-2");

    // ---- 6. 暂停与断点续传：不重传已校验块 ----
    std::env::set_var("HARBOR_TRANSFER_WORKERS", "1");
    let big = random_bytes(64 * 1024 * 1024 + 12_345, 1234);
    fs::write(shared.join("big.bin"), &big).unwrap();
    let detail = dest
        .create_sync_task(CreateTaskRequest {
            session_id: session_id.clone(),
            share_ids: vec![share.id.clone()],
            dest_root: dest_root.to_string_lossy().to_string(),
            mode: SyncMode::Delta,
            conflict: ConflictPolicy::Overwrite,
            verify_strict: false,
        })
        .await
        .unwrap();
    let task_id = detail.summary.id.clone();
    let mut events = dest.subscribe();
    dest.start_sync_task(&task_id).await.unwrap();

    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let mut paused = false;
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match timeout(remaining, events.recv()).await {
            Ok(Ok(CoreEvent::TaskProgress { task, .. })) => {
                if task.id == task_id
                    && task.state == TaskState::Running
                    && task.done_bytes > task.total_bytes / 2
                {
                    dest.pause_sync_task(&task_id).await.unwrap();
                    paused = true;
                    break;
                }
            }
            Ok(Ok(CoreEvent::TaskFinished { task, .. })) if task.id == task_id => break,
            Ok(Ok(_)) => {}
            Ok(Err(err)) => panic!("事件通道错误：{err}"),
            Err(_) => break,
        }
    }
    assert!(paused, "大文件应在传输完成前暂停");

    let part_dir = target.join(".harbor/parts").join(&task_id);
    assert!(part_dir.is_dir(), "断点目录应存在");
    assert!(
        fs::read_dir(&part_dir).unwrap().flatten().any(|item| item
            .path()
            .extension()
            .and_then(|ext| ext.to_str())
            == Some("json")),
        "断点状态应已落盘"
    );
    let paused_detail = dest.get_sync_task(&task_id).await.unwrap();
    assert_eq!(paused_detail.summary.state, TaskState::Paused);
    assert!(paused_detail.summary.done_bytes > 0);

    let mut resumed = false;
    for _ in 0..100 {
        if dest.start_sync_task(&task_id).await.is_ok() {
            resumed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(resumed, "暂停后应可继续");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
    loop {
        let current = dest.get_sync_task(&task_id).await.unwrap();
        match current.summary.state {
            TaskState::Done => break,
            TaskState::Interrupted | TaskState::Failed | TaskState::Cancelled => {
                panic!("恢复失败：{}", current.summary.message)
            }
            _ => {}
        }
        if tokio::time::Instant::now() > deadline {
            panic!("恢复超时");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(fs::read(target.join("big.bin")).unwrap(), big);
    let finished = dest.get_sync_task(&task_id).await.unwrap();
    let report = finished.report.expect("完成后应生成报告");
    assert!(
        report.resumed_bytes > 0,
        "续传应统计已跳过重传的字节：{:?}",
        report
    );
    assert!(
        !walkdir::WalkDir::new(&target)
            .into_iter()
            .flatten()
            .any(|entry| entry.path().extension().and_then(|ext| ext.to_str()) == Some("part")),
        "恢复完成后不应残留 .part"
    );

    // ---- 6. 数据连接必须凭一次性票据 ----
    assert!(
        RemoteData::connect(
            "127.0.0.1",
            info.port,
            None,
            "deadbeef",
            CancellationToken::new()
        )
        .await
        .is_err(),
        "无效票据必须被拒绝"
    );

    // ---- 7. 非法路径 / 不存在共享由源端拒绝 ----
    let err = dest
        .remote_browse(&session_id, &share.id, "../etc")
        .await
        .unwrap_err();
    assert!(matches!(err, HarborError::InvalidPath));

    source.stop_source().await;
    fs::remove_dir_all(&root).ok();
}
