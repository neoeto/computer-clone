//! 接收端传输引擎：清单比对、分块落盘、断点恢复、冲突与镜像清理。
//!
//! 单文件流程：
//! 1. 数据连接 `file_open` 获取源端块清单与内容哈希；
//! 2. 与本地文件按「大小 + mtime → 内容哈希」逐级比对；
//! 3. 在 `<目标共享目录>/.harbor/parts/<task>/` 创建 `.part`，按缺失位图请求块；
//! 4. 每块校验 BLAKE3 后 `write_at`，每批更新位图与进度；
//! 5. `fsync` → 设置 mtime/权限 → 旧文件移入隔离区 → 原子 `rename`。

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use fs2::available_space;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use walkdir::WalkDir;

use crate::chunker;
use crate::data::{self, RemoteData, CHUNK_BATCH};
use crate::engine::Engine;
use crate::error::{HarborError, Result};
use crate::shares;
use crate::sync::{
    chunk_offsets, conflict_name, join_components, safe_rel_components, safe_share_dir_name,
    unique_dest_path, utc_date, ConflictPolicy, FileProgress, FileState, ManifestPage, PartState,
    RemoteFileHeader, SyncMode, TaskFailure, TaskReport, TaskShare, TaskState, TaskSummary,
};
use crate::tasks::{
    add_file_bytes, part_key, record_failure, set_file_state, StopReason, TaskManager,
};
use crate::types::CoreEvent;
use crate::util::now_ms;

const DISK_RESERVE: u64 = 512 * 1024 * 1024;
const DEFAULT_WORKERS: usize = 3;

struct Job {
    index: usize,
    share_id: String,
    share_dir: String,
    share_root: PathBuf,
    source_path: String,
    dest_rel: String,
    target: PathBuf,
}

pub(crate) async fn run_task(engine: Arc<Engine>, task_id: String, cancel: CancellationToken) {
    let result = run_task_inner(&engine, &task_id, &cancel).await;
    let manager = &engine.tasks;
    let mut finished: Option<(TaskSummary, TaskReport)> = None;

    if let Ok(entry) = manager.entry(&task_id).await {
        let mut guard = entry.lock().await;
        let now = now_ms();
        let stop = guard.stop_reason;
        match stop {
            Some(StopReason::Pause) => {
                guard.persisted.summary.state = TaskState::Paused;
                guard.persisted.summary.message =
                    "已暂停 · 已传输部分保存在断点目录，可继续".into();
            }
            Some(StopReason::Cancel) => {
                guard.persisted.summary.state = TaskState::Cancelled;
                guard.persisted.summary.message = "已取消，断点数据已保留".into();
            }
            Some(StopReason::Interrupt) => {
                guard.persisted.summary.state = TaskState::Interrupted;
            }
            None => match result {
                Ok(report) => {
                    {
                        let summary = &mut guard.persisted.summary;
                        summary.state = TaskState::Done;
                        summary.finished_at = Some(now);
                        summary.message = if summary.failed_files > 0 {
                            format!(
                                "同步完成，{} 个文件失败，可重试失败项",
                                summary.failed_files
                            )
                        } else if summary.total_files == 0 {
                            "同步完成，没有需要传输的文件".into()
                        } else {
                            "同步完成，所有文件均已校验".into()
                        };
                        summary.speed_bps = 0;
                        summary.eta_secs = Some(0);
                        summary.percent = if summary.failed_files > 0 {
                            crate::sync::compute_percent(
                                summary.done_bytes,
                                summary.total_bytes,
                                summary.done_files,
                                summary.skipped_files,
                                summary.failed_files,
                                summary.total_files,
                            )
                        } else {
                            1.0
                        };
                    }
                    guard.persisted.report = Some(report.clone());
                    finished = Some((guard.persisted.summary.clone(), report));
                }
                Err(err) => {
                    let summary = &mut guard.persisted.summary;
                    summary.state = TaskState::Interrupted;
                    summary.finished_at = Some(now);
                    summary.speed_bps = 0;
                    summary.eta_secs = None;
                    summary.message = format!("同步中断：{err}");
                }
            },
        }
        guard.persisted.summary.updated_at = now;
        guard.persisted.summary.refresh_progress();
        if let Some((task, _)) = finished.as_mut() {
            *task = guard.persisted.summary.clone();
        }
    }

    let _ = manager.save(&task_id).await;
    manager.publish_state(&task_id).await;
    if let Some((task, report)) = finished {
        let _ = manager
            .events()
            .send(CoreEvent::TaskFinished { task, report });
    }
    manager.end_run(&task_id).await;
}

async fn run_task_inner(
    engine: &Arc<Engine>,
    task_id: &str,
    cancel: &CancellationToken,
) -> Result<TaskReport> {
    let manager = &engine.tasks;
    let persisted = {
        let entry = manager.entry(task_id).await?;
        let guard = entry.lock().await;
        guard.persisted.clone()
    };
    let summary = &persisted.summary;
    if cancel.is_cancelled() {
        return Err(HarborError::Cancelled);
    }

    let info = engine.session_info(&summary.session_id).await?;
    if info.device.id != summary.peer_device_id {
        return Err(HarborError::Other("当前连接的不是任务对应的源端".into()));
    }
    let remote_shares = engine.remote_list_shares(&summary.session_id).await?;
    let mut selected: Vec<(TaskShare, u64, u64)> = Vec::new();
    for share in &persisted.shares {
        let remote = remote_shares
            .iter()
            .find(|remote| remote.id == share.id)
            .ok_or_else(|| HarborError::NotFound(share.name.clone()))?;
        selected.push((share.clone(), remote.file_count, remote.total_bytes));
    }

    let dest_root = PathBuf::from(&summary.dest_root);
    std::fs::create_dir_all(&dest_root)
        .map_err(|err| HarborError::Storage(format!("创建保存位置失败：{err}")))?;
    let dest_root = dest_root
        .canonicalize()
        .map_err(|err| HarborError::Storage(format!("保存位置不可用：{err}")))?;

    let planned_bytes: u64 = selected.iter().map(|(_, _, bytes)| *bytes).sum();
    if let Ok(available) = available_space(&dest_root) {
        if available < planned_bytes.saturating_add(DISK_RESERVE) {
            return Err(HarborError::Storage(format!(
                "磁盘空间不足：预计需要 {}，当前可用 {}",
                human_bytes(planned_bytes.saturating_add(DISK_RESERVE)),
                human_bytes(available)
            )));
        }
    }

    // 扫描阶段：拉取清单、创建目录、做初级比对。
    let mut files: Vec<FileProgress> = Vec::new();
    let mut jobs: Vec<Job> = Vec::new();
    let mut expected: HashMap<String, HashSet<String>> = HashMap::new();
    let mut failures: Vec<TaskFailure> = Vec::new();
    let share_dirs = crate::sync::share_directory_names(&persisted.shares);
    let mut skipped_files = 0u64;
    let mut failed_files = 0u64;
    let mut skipped_bytes = 0u64;
    let mut renamed = 0u64;
    let mut totals = (0u64, 0u64);

    for (share_index, (share, _, _)) in selected.iter().enumerate() {
        if cancel.is_cancelled() {
            return Err(HarborError::Cancelled);
        }
        let share_dir = share_dirs
            .get(share_index)
            .cloned()
            .unwrap_or_else(|| safe_share_dir_name(&share.name));
        let share_root = ensure_dir(&dest_root, std::slice::from_ref(&share_dir))?;
        let manifest = fetch_manifest(engine, &summary.session_id, &share.id, cancel).await?;
        totals.0 += manifest.total_files;
        totals.1 += manifest.total_bytes;

        let mut used = BTreeSet::new();
        let mut expected_paths = HashSet::new();
        for (seen, entry) in manifest.entries.iter().enumerate() {
            if seen % 256 == 0 && cancel.is_cancelled() {
                return Err(HarborError::Cancelled);
            }

            if entry.is_dir {
                let parts = match safe_rel_components(&entry.path) {
                    Ok(parts) => parts,
                    Err(_) => continue,
                };
                if parts.iter().any(|part| shares::is_ignored_name(part)) {
                    continue;
                }
                ensure_dir(&share_root, &parts)?;
                expected_paths.insert(parts.join("/"));
                continue;
            }

            let parts = match safe_rel_components(&entry.path) {
                Ok(parts) => parts,
                Err(_) => {
                    failures.push(TaskFailure {
                        share_id: share.id.clone(),
                        path: entry.path.clone(),
                        message: "文件名不合法，已跳过".into(),
                        at: now_ms(),
                    });
                    failed_files += 1;
                    continue;
                }
            };
            if parts.iter().any(|part| shares::is_ignored_name(part)) {
                continue;
            }
            let (dest_parts, was_renamed) = unique_dest_path(&parts, &mut used)?;
            if was_renamed {
                renamed += 1;
            }
            let dest_rel = dest_parts.join("/");
            expected_paths.insert(dest_rel.clone());
            let target = join_components(&share_root, &dest_parts);
            let dest_display = format!("{share_dir}/{dest_rel}");

            let mut state = FileState::Queued;
            let mut error = None;
            match std::fs::symlink_metadata(&target) {
                Ok(meta) if meta.is_dir() => {
                    state = FileState::Failed;
                    error = Some("目标路径已存在同名文件夹".into());
                }
                Ok(meta) if meta.file_type().is_symlink() => {
                    state = FileState::Failed;
                    error = Some("目标路径是符号链接，已跳过".into());
                }
                Ok(meta) => {
                    if summary.mode == SyncMode::NewOnly
                        || (meta.len() == entry.size && mtime_matches(&meta, entry.mtime_ms))
                    {
                        state = FileState::Skipped;
                    }
                }
                Err(_) => {}
            }

            let key = format!("{}\u{1f}{}", share.id, entry.path);
            let file_index = files.len();
            if state == FileState::Failed {
                failed_files += 1;
                failures.push(TaskFailure {
                    share_id: share.id.clone(),
                    path: entry.path.clone(),
                    message: error.clone().unwrap_or_default(),
                    at: now_ms(),
                });
            } else if state == FileState::Skipped {
                skipped_files += 1;
                skipped_bytes = skipped_bytes.saturating_add(entry.size);
            }
            files.push(FileProgress {
                key: key.clone(),
                share_id: share.id.clone(),
                path: entry.path.clone(),
                dest_path: dest_display,
                size: entry.size,
                done_bytes: if state == FileState::Skipped {
                    entry.size
                } else {
                    0
                },
                state,
                error,
                resumed: false,
            });
            if state == FileState::Queued {
                jobs.push(Job {
                    index: file_index,
                    share_id: share.id.clone(),
                    share_dir: share_dir.clone(),
                    share_root: share_root.clone(),
                    source_path: entry.path.clone(),
                    dest_rel,
                    target,
                });
            }
        }
        expected.insert(share.id.clone(), expected_paths);
    }

    {
        let entry = manager.entry(task_id).await?;
        let mut guard = entry.lock().await;
        guard.files = files;
        guard.persisted.failures = failures;
        guard.renamed = renamed;
        let done_bytes = skipped_bytes;
        {
            let summary = &mut guard.persisted.summary;
            summary.total_files = totals.0;
            summary.total_bytes = totals.1;
            summary.done_files = 0;
            summary.skipped_files = skipped_files;
            summary.failed_files = failed_files;
            summary.conflict_files = 0;
            summary.done_bytes = done_bytes;
            summary.state = TaskState::Running;
            summary.message = if jobs.is_empty() {
                "没有需要传输的文件".into()
            } else {
                format!("正在传输 {} 个文件…", jobs.len())
            };
            summary.updated_at = now_ms();
            summary.percent = crate::sync::compute_percent(
                summary.done_bytes,
                summary.total_bytes,
                summary.done_files,
                summary.skipped_files,
                summary.failed_files,
                summary.total_files,
            );
        }
        guard.speed.reset(done_bytes);
    }
    manager.save(task_id).await?;
    manager.publish_state(task_id).await;
    manager.publish(task_id, true).await;

    if !jobs.is_empty() {
        let workers = std::env::var("HARBOR_TRANSFER_WORKERS")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(DEFAULT_WORKERS)
            .clamp(1, 4);
        let queue = Arc::new(JobQueue::new(jobs));
        let mut set: JoinSet<Result<()>> = JoinSet::new();
        for _ in 0..workers {
            let engine = engine.clone();
            let task_id = task_id.to_string();
            let session_id = summary.session_id.clone();
            let cancel = cancel.clone();
            let queue = queue.clone();
            set.spawn(async move { worker_loop(engine, task_id, session_id, queue, cancel).await });
        }

        let mut fatal: Option<HarborError> = None;
        while let Some(joined) = set.join_next().await {
            match joined {
                Ok(Ok(())) => {}
                Ok(Err(err)) => {
                    if !matches!(err, HarborError::Cancelled) && fatal.is_none() {
                        fatal = Some(err);
                    }
                    cancel.cancel();
                }
                Err(err) => {
                    if fatal.is_none() {
                        fatal = Some(HarborError::Other(format!("传输任务异常退出：{err}")));
                    }
                    cancel.cancel();
                }
            }
        }
        if let Some(err) = fatal {
            return Err(err);
        }
        if cancel.is_cancelled() {
            return Err(HarborError::Cancelled);
        }
    }

    // 镜像清理：只在没有失败项时执行，避免“传输失败 + 删除旧文件”双重损失。
    if summary.mode == SyncMode::Mirror {
        let failed = manager
            .entry(task_id)
            .await?
            .lock()
            .await
            .persisted
            .summary
            .failed_files;
        if failed == 0 {
            let stamp = manager.entry(task_id).await?.lock().await.run_stamp;
            for (share_index, (share, _, _)) in selected.iter().enumerate() {
                if cancel.is_cancelled() {
                    return Err(HarborError::Cancelled);
                }
                let share_dir = share_dirs
                    .get(share_index)
                    .cloned()
                    .unwrap_or_else(|| safe_share_dir_name(&share.name));
                let share_root = dest_root.join(&share_dir);
                let Some(paths) = expected.get(&share.id) else {
                    continue;
                };
                mirror_cleanup(
                    engine,
                    task_id,
                    &share_dir,
                    &share_root,
                    paths,
                    stamp,
                    cancel,
                )
                .await?;
            }
        }
    }

    if cancel.is_cancelled() {
        return Err(HarborError::Cancelled);
    }

    let entry = manager.entry(task_id).await?;
    let guard = entry.lock().await;
    let summary = &guard.persisted.summary;
    let duration = summary
        .started_at
        .map(|start| now_ms().saturating_sub(start))
        .unwrap_or(0);
    let average_bps = if duration > 0 {
        summary.done_bytes.saturating_mul(1000) / duration
    } else {
        0
    };
    let report = TaskReport {
        duration_ms: duration,
        average_bps,
        transferred_files: summary.done_files,
        skipped_files: summary.skipped_files,
        failed_files: summary.failed_files,
        conflicts: summary.conflict_files,
        resumed_bytes: guard.resumed_bytes,
        verified_bytes: guard.verified_bytes,
        renamed: guard.renamed,
        trash: guard.trash.clone(),
    };
    Ok(report)
}

async fn fetch_manifest(
    engine: &Engine,
    session_id: &str,
    share_id: &str,
    cancel: &CancellationToken,
) -> Result<ManifestPage> {
    let mut entries = Vec::new();
    let mut cursor: Option<String> = None;
    let mut totals: (u64, u64);
    loop {
        if cancel.is_cancelled() {
            return Err(HarborError::Cancelled);
        }
        let page = engine
            .remote_manifest(session_id, share_id, cursor.clone())
            .await?;
        if page.share_id != share_id {
            return Err(HarborError::Protocol("清单所属共享目录不一致".into()));
        }
        totals = (page.total_files, page.total_bytes);
        entries.extend(page.entries);
        match page.next {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    Ok(ManifestPage {
        share_id: share_id.to_string(),
        entries,
        next: None,
        total_files: totals.0,
        total_bytes: totals.1,
    })
}

/* ------------------------------------------------------------------ */
/* 工作队列                                                            */
/* ------------------------------------------------------------------ */

struct JobQueue {
    jobs: tokio::sync::Mutex<Vec<Job>>,
}

impl JobQueue {
    fn new(mut jobs: Vec<Job>) -> Self {
        jobs.reverse();
        Self {
            jobs: tokio::sync::Mutex::new(jobs),
        }
    }

    async fn pop(&self) -> Option<Job> {
        self.jobs.lock().await.pop()
    }
}

async fn worker_loop(
    engine: Arc<Engine>,
    task_id: String,
    session_id: String,
    queue: Arc<JobQueue>,
    cancel: CancellationToken,
) -> Result<()> {
    let mut client: Option<RemoteData> = None;
    while let Some(job) = queue.pop().await {
        if cancel.is_cancelled() {
            break;
        }
        if client.is_none() {
            let conn = engine.open_remote_data(&session_id, cancel.clone()).await?;
            client = Some(conn);
        }
        let mut conn = client.take().expect("数据连接已建立");
        let result = transfer_file(&engine, &task_id, job, &mut conn, &cancel).await;
        match result {
            Ok(()) => {
                client = Some(conn);
            }
            Err(HarborError::Cancelled) => {
                break;
            }
            Err(err) => {
                if data::is_network_error(&err)
                    || matches!(err, HarborError::Storage(_))
                    || matches!(err, HarborError::Protocol(_))
                {
                    return Err(err);
                }
                client = Some(conn);
            }
        }
    }
    Ok(())
}

/* ------------------------------------------------------------------ */
/* 单文件传输                                                          */
/* ------------------------------------------------------------------ */

async fn transfer_file(
    engine: &Arc<Engine>,
    task_id: &str,
    job: Job,
    conn: &mut RemoteData,
    cancel: &CancellationToken,
) -> Result<()> {
    let result = transfer_file_inner(engine, task_id, &job, conn, cancel).await;
    if let Err(err) = &result {
        if !matches!(err, HarborError::Cancelled) {
            fail_file(engine, task_id, job.index, err.to_string()).await;
        }
    }
    result
}

async fn transfer_file_inner(
    engine: &Arc<Engine>,
    task_id: &str,
    job: &Job,
    conn: &mut RemoteData,
    cancel: &CancellationToken,
) -> Result<()> {
    if cancel.is_cancelled() {
        return Err(HarborError::Cancelled);
    }
    let manager = &engine.tasks;
    let (mode, conflict, peer_name, verify_strict) = {
        let entry = manager.entry(task_id).await?;
        let guard = entry.lock().await;
        let summary = &guard.persisted.summary;
        (
            summary.mode,
            summary.conflict,
            summary.peer_name.clone(),
            summary.verify_strict,
        )
    };

    let header = conn.open_file(&job.share_id, &job.source_path).await?;
    let content_hash = header.content_hash_bytes()?;
    let derived: Vec<[u8; 32]> = header
        .chunks
        .iter()
        .map(|chunk| chunk.hash_bytes())
        .collect::<Result<Vec<_>>>()?;
    if chunker::content_hash(derived.iter()) != content_hash {
        return Err(HarborError::Protocol("源端块清单与文件哈希不一致".into()));
    }

    manager
        .set_transferring(task_id, job.index, header.size)
        .await;

    let mut target = job.target.clone();
    let existing = std::fs::symlink_metadata(&target).ok();
    if let Some(meta) = existing.as_ref() {
        if meta.is_dir() {
            return Err(HarborError::Conflict("目标路径是文件夹".into()));
        }
        if meta.file_type().is_symlink() {
            return Err(HarborError::InvalidPath);
        }
        if mode == SyncMode::NewOnly {
            manager
                .finish_file(task_id, job.index, FileState::Skipped)
                .await;
            let _ = conn.close_file().await;
            return Ok(());
        }
        if meta.len() == header.size && mtime_matches(meta, header.mtime_ms) {
            manager
                .finish_file(task_id, job.index, FileState::Skipped)
                .await;
            let _ = conn.close_file().await;
            return Ok(());
        }
        if meta.len() == header.size {
            let path = target.clone();
            let local = tokio::task::spawn_blocking(move || chunker::chunk_file(&path))
                .await
                .map_err(|err| HarborError::Other(format!("本地校验失败：{err}")))??;
            if local.content_hash == content_hash {
                // 内容已经一致；mtime 设置失败不应让文件进入失败列表。
                let _ = apply_metadata(&target, &header, true);
                manager
                    .finish_file(task_id, job.index, FileState::Skipped)
                    .await;
                let _ = conn.close_file().await;
                return Ok(());
            }
        }

        match conflict {
            ConflictPolicy::Skip => {
                manager
                    .finish_file_with_message(
                        task_id,
                        job.index,
                        FileState::Conflict,
                        "本地文件内容不同，按冲突策略跳过",
                    )
                    .await;
                let _ = conn.close_file().await;
                return Ok(());
            }
            ConflictPolicy::KeepBoth => {
                target = unique_conflict_target(&target, &peer_name);
                let new_rel = target
                    .strip_prefix(&job.share_root)
                    .map(|path| path.to_string_lossy().replace('\\', "/"))
                    .unwrap_or_else(|_| job.dest_rel.clone());
                manager
                    .update_dest_path(task_id, job.index, format!("{}/{}", job.share_dir, new_rel))
                    .await;
            }
            ConflictPolicy::Overwrite => {}
        }
    }

    let part_dir = job.share_root.join(".harbor").join("parts").join(task_id);
    ensure_dir_absolute(&job.share_root, &part_dir)?;
    let key = part_key(&job.share_id, &job.source_path);
    let part_path = part_dir.join(format!("{key}.part"));
    let state_path = part_dir.join(format!("{key}.json"));

    let mut state = read_part_state(&state_path);
    if state
        .as_ref()
        .map(|state| !state.matches(&header))
        .unwrap_or(true)
    {
        state = None;
    }
    if state.is_some() {
        let valid = std::fs::metadata(&part_path)
            .map(|meta| meta.len() == header.size)
            .unwrap_or(false);
        if !valid {
            state = None;
        }
    }
    if state.is_none() {
        let _ = std::fs::remove_file(&part_path);
        let _ = std::fs::remove_file(&state_path);
    }

    let mut state = state.unwrap_or_else(|| PartState::new(&header));
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&part_path)
        .await?;
    file.set_len(header.size).await?;

    let offsets = chunk_offsets(&header.chunks);
    let mut bitmap = state.bitmap();
    let mut pending: Vec<u32> = Vec::new();
    let mut received = 0u64;

    // 断点恢复时复核已标记的块。进程崩溃通常不会丢页缓存，但系统掉电/磁盘错误
    // 可能让“位图已记录、数据未落盘”，这里把校验失败的块重新加入待传列表。
    for (index, info) in header.chunks.iter().enumerate() {
        let index = index as u32;
        if !bitmap.get(index) {
            pending.push(index);
            continue;
        }
        let data = read_at(&mut file, offsets[index as usize], info.len as usize).await?;
        if chunker::chunk_hash(&data) == info.hash_bytes()? {
            received = received.saturating_add(info.len as u64);
        } else {
            bitmap.clear(index);
            pending.push(index);
        }
    }
    state.set_bitmap(&bitmap);
    if received > 0 {
        manager.record_resumed(task_id, job.index, received).await;
    }

    let mut bad_attempts: HashMap<u32, u8> = HashMap::new();

    while !pending.is_empty() {
        if cancel.is_cancelled() {
            persist_part_state(&state_path, &mut state, &bitmap)?;
            return Err(HarborError::Cancelled);
        }
        let take = pending.len().min(CHUNK_BATCH);
        let batch: Vec<u32> = pending.drain(..take).collect();
        let chunks = match conn.request_chunks(&batch).await {
            Ok(chunks) => chunks,
            Err(err) => {
                persist_part_state(&state_path, &mut state, &bitmap)?;
                return Err(err);
            }
        };
        let mut retry = Vec::new();
        for (index, data) in chunks {
            let Some(info) = header.chunks.get(index as usize) else {
                continue;
            };
            let hash = chunker::chunk_hash(&data);
            if data.len() != info.len as usize || hash != info.hash_bytes()? {
                let attempts = bad_attempts.entry(index).or_insert(0);
                *attempts += 1;
                if *attempts >= 3 {
                    persist_part_state(&state_path, &mut state, &bitmap)?;
                    return Err(HarborError::Protocol(format!(
                        "块 {index} 连续 3 次校验失败"
                    )));
                }
                retry.push(index);
                continue;
            }
            file.seek(std::io::SeekFrom::Start(offsets[index as usize]))
                .await?;
            file.write_all(&data).await?;
            bitmap.set(index);
            received = received.saturating_add(data.len() as u64);
            add_file_bytes_async(manager, task_id, job.index, received).await;
        }
        if !retry.is_empty() {
            pending.extend(retry);
        }
        persist_part_state(&state_path, &mut state, &bitmap)?;
        manager.publish(task_id, false).await;
        manager.save_if_due(task_id).await;
    }

    file.flush().await?;
    file.sync_all().await?;
    drop(file);

    if verify_strict {
        let path = part_path.clone();
        let verify = tokio::task::spawn_blocking(move || chunker::chunk_file(&path))
            .await
            .map_err(|err| HarborError::Other(format!("严格校验失败：{err}")))??;
        if verify.size != header.size || verify.content_hash != content_hash {
            let _ = std::fs::remove_file(&part_path);
            let _ = std::fs::remove_file(&state_path);
            return Err(HarborError::Storage(
                "落盘文件严格校验不通过，已丢弃".into(),
            ));
        }
        manager.record_verified(task_id, verify.size).await;
    }

    apply_metadata(&part_path, &header, false)?;
    if let Ok(meta) = std::fs::symlink_metadata(&target) {
        if meta.is_dir() || meta.file_type().is_symlink() {
            return Err(HarborError::Conflict("目标路径被占用".into()));
        }
        if conflict == ConflictPolicy::Overwrite || mode == SyncMode::Mirror {
            move_to_trash(&job.share_root, &target, job, manager, task_id).await?;
        }
    }
    if let Some(parent) = target.parent() {
        ensure_dir_absolute(&job.share_root, parent)?;
    }
    if let Err(err) = std::fs::rename(&part_path, &target) {
        // Windows 上目标可能被其他进程短暂占用。
        if target.exists() {
            move_to_trash(&job.share_root, &target, job, manager, task_id).await?;
            std::fs::rename(&part_path, &target)
                .map_err(|err| HarborError::Storage(format!("写入目标文件失败：{err}")))?;
        } else {
            return Err(HarborError::Storage(format!("写入目标文件失败：{err}")));
        }
    }
    let _ = std::fs::remove_file(&state_path);
    manager
        .finish_file(task_id, job.index, FileState::Done)
        .await;
    manager.publish(task_id, false).await;
    let _ = conn.close_file().await;
    Ok(())
}

async fn mirror_cleanup(
    engine: &Arc<Engine>,
    task_id: &str,
    share_dir: &str,
    share_root: &Path,
    expected: &HashSet<String>,
    stamp: u64,
    cancel: &CancellationToken,
) -> Result<()> {
    if !share_root.is_dir() {
        return Ok(());
    }
    let manager = &engine.tasks;
    let mut dirs = Vec::new();
    let mut to_trash = Vec::new();
    for item in WalkDir::new(share_root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| {
            let name = entry.file_name().to_string_lossy();
            entry.depth() == 0 || !shares::is_ignored_name(&name)
        })
        .flatten()
    {
        if cancel.is_cancelled() {
            return Err(HarborError::Cancelled);
        }
        if item.depth() == 0 || item.file_type().is_symlink() {
            continue;
        }
        if item.file_type().is_dir() {
            dirs.push(item.path().to_path_buf());
            continue;
        }
        if !item.file_type().is_file() {
            continue;
        }
        let Ok(relative) = item.path().strip_prefix(share_root) else {
            continue;
        };
        let rel = relative.to_string_lossy().replace('\\', "/");
        if !expected.contains(&rel) {
            to_trash.push(item.path().to_path_buf());
        }
    }

    for path in to_trash {
        let Ok(relative) = path.strip_prefix(share_root) else {
            continue;
        };
        let trash_path = share_root
            .join(".harbor-trash")
            .join(stamp.to_string())
            .join(relative);
        if let Some(parent) = trash_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        match std::fs::rename(&path, &trash_path) {
            Ok(()) => {
                manager
                    .record_trash(
                        task_id,
                        format!(
                            "{share_dir}/.harbor-trash/{stamp}/{}",
                            relative.to_string_lossy().replace('\\', "/")
                        ),
                    )
                    .await;
            }
            Err(err) => {
                tracing::warn!(?err, path = %path.display(), "镜像删除失败");
                manager
                    .record_failure_message(
                        task_id,
                        format!("无法移入隔离区：{err}"),
                        relative.to_string_lossy().to_string(),
                    )
                    .await;
            }
        }
    }

    dirs.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    for dir in dirs {
        let _ = std::fs::remove_dir(dir);
    }
    Ok(())
}

/* ------------------------------------------------------------------ */
/* TaskEntry 上的细粒度更新                                            */
/* ------------------------------------------------------------------ */

async fn add_file_bytes_async(manager: &TaskManager, task_id: &str, index: usize, absolute: u64) {
    if let Ok(entry) = manager.entry(task_id).await {
        let mut guard = entry.lock().await;
        add_file_bytes(&mut guard, index, absolute);
    }
}

async fn fail_file(engine: &Arc<Engine>, task_id: &str, index: usize, message: String) {
    let manager = &engine.tasks;
    if let Ok(entry) = manager.entry(task_id).await {
        let mut guard = entry.lock().await;
        if matches!(
            guard.files.get(index).map(|file| file.state),
            Some(FileState::Done | FileState::Skipped | FileState::Conflict | FileState::Failed)
        ) {
            return;
        }
        record_failure(&mut guard, index, message);
    }
    manager.publish(task_id, true).await;
    manager.save_if_due(task_id).await;
}

impl TaskManager {
    async fn set_transferring(&self, task_id: &str, index: usize, size: u64) {
        if let Ok(entry) = self.entry(task_id).await {
            let mut guard = entry.lock().await;
            let old_size = guard.files.get(index).map(|file| file.size);
            if let Some(old_size) = old_size {
                if old_size != size {
                    if size > old_size {
                        guard.persisted.summary.total_bytes += size - old_size;
                    } else {
                        guard.persisted.summary.total_bytes = guard
                            .persisted
                            .summary
                            .total_bytes
                            .saturating_sub(old_size - size);
                    }
                    if let Some(file) = guard.files.get_mut(index) {
                        file.size = size;
                    }
                }
            }
            set_file_state(&mut guard, index, FileState::Transferring, None);
        }
        self.publish(task_id, false).await;
    }

    async fn finish_file(&self, task_id: &str, index: usize, state: FileState) {
        if let Ok(entry) = self.entry(task_id).await {
            let mut guard = entry.lock().await;
            set_file_state(&mut guard, index, state, None);
        }
        self.publish(task_id, false).await;
    }

    async fn finish_file_with_message(
        &self,
        task_id: &str,
        index: usize,
        state: FileState,
        message: &str,
    ) {
        if let Ok(entry) = self.entry(task_id).await {
            let mut guard = entry.lock().await;
            set_file_state(&mut guard, index, state, Some(message.to_string()));
        }
        self.publish(task_id, false).await;
    }

    async fn update_dest_path(&self, task_id: &str, index: usize, dest: String) {
        if let Ok(entry) = self.entry(task_id).await {
            let mut guard = entry.lock().await;
            if let Some(file) = guard.files.get_mut(index) {
                file.dest_path = dest;
            }
        }
    }

    async fn record_resumed(&self, task_id: &str, index: usize, bytes: u64) {
        if let Ok(entry) = self.entry(task_id).await {
            let mut guard = entry.lock().await;
            add_file_bytes(&mut guard, index, bytes);
            if let Some(file) = guard.files.get_mut(index) {
                file.resumed = true;
            }
            guard.resumed_bytes = guard.resumed_bytes.saturating_add(bytes);
        }
        self.publish(task_id, false).await;
    }

    async fn record_verified(&self, task_id: &str, bytes: u64) {
        if let Ok(entry) = self.entry(task_id).await {
            let mut guard = entry.lock().await;
            guard.verified_bytes = guard.verified_bytes.saturating_add(bytes);
        }
    }

    async fn record_trash(&self, task_id: &str, path: String) {
        if let Ok(entry) = self.entry(task_id).await {
            entry.lock().await.trash.push(path);
        }
    }

    async fn record_failure_message(&self, task_id: &str, message: String, path: String) {
        if let Ok(entry) = self.entry(task_id).await {
            let mut guard = entry.lock().await;
            guard.persisted.failures.push(TaskFailure {
                share_id: String::new(),
                path,
                message,
                at: now_ms(),
            });
            guard.persisted.summary.failed_files += 1;
        }
        self.publish(task_id, true).await;
    }
}

/* ------------------------------------------------------------------ */
/* 文件系统辅助                                                        */
/* ------------------------------------------------------------------ */

fn ensure_dir(root: &Path, parts: &[String]) -> Result<PathBuf> {
    let mut current = root.to_path_buf();
    for part in parts {
        current.push(part);
        match std::fs::symlink_metadata(&current) {
            Ok(meta) => {
                if meta.file_type().is_symlink() {
                    return Err(HarborError::InvalidPath);
                }
                if !meta.is_dir() {
                    return Err(HarborError::Conflict(format!(
                        "目标路径被非文件夹占用：{}",
                        current.display()
                    )));
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                match std::fs::create_dir(&current) {
                    Ok(()) => {}
                    // 多个传输 worker 可能同时创建同一个临时目录。
                    Err(create_err) if create_err.kind() == std::io::ErrorKind::AlreadyExists => {
                        let meta = std::fs::symlink_metadata(&current)?;
                        if meta.file_type().is_symlink() || !meta.is_dir() {
                            return Err(HarborError::InvalidPath);
                        }
                    }
                    Err(create_err) => {
                        return Err(HarborError::Storage(format!("创建目录失败：{create_err}")))
                    }
                }
            }
            Err(err) => return Err(err.into()),
        }
    }
    Ok(current)
}

fn ensure_dir_absolute(root: &Path, path: &Path) -> Result<()> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| HarborError::InvalidPath)?;
    let mut cursor = root.to_path_buf();
    for component in relative.components() {
        cursor.push(component);
        match std::fs::symlink_metadata(&cursor) {
            Ok(meta) => {
                if meta.file_type().is_symlink() {
                    return Err(HarborError::InvalidPath);
                }
                if !meta.is_dir() {
                    return Err(HarborError::Conflict(format!(
                        "目标路径被非文件夹占用：{}",
                        cursor.display()
                    )));
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                match std::fs::create_dir(&cursor) {
                    Ok(()) => {}
                    Err(create_err) if create_err.kind() == std::io::ErrorKind::AlreadyExists => {
                        let meta = std::fs::symlink_metadata(&cursor)?;
                        if meta.file_type().is_symlink() || !meta.is_dir() {
                            return Err(HarborError::InvalidPath);
                        }
                    }
                    Err(create_err) => {
                        return Err(HarborError::Storage(format!("创建目录失败：{create_err}")))
                    }
                }
            }
            Err(err) => return Err(err.into()),
        }
    }
    Ok(())
}

fn mtime_matches(meta: &std::fs::Metadata, source_mtime: Option<u64>) -> bool {
    let Some(source) = source_mtime else {
        return false;
    };
    let Some(modified) = meta
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis() as u64)
    else {
        return false;
    };
    modified.abs_diff(source) <= 1
}

fn apply_metadata(path: &Path, header: &RemoteFileHeader, only_mtime: bool) -> Result<()> {
    if let Some(mtime) = header.mtime_ms {
        let time = UNIX_EPOCH
            .checked_add(Duration::from_millis(mtime))
            .unwrap_or(UNIX_EPOCH);
        let file = std::fs::OpenOptions::new().write(true).open(path)?;
        file.set_modified(time)
            .map_err(|err| HarborError::Storage(format!("设置修改时间失败：{err}")))?;
    }
    if only_mtime {
        return Ok(());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if header.mode != 0 {
            let mode = header.mode & 0o777;
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
        }
    }
    Ok(())
}

async fn read_at(file: &mut tokio::fs::File, offset: u64, len: usize) -> Result<Vec<u8>> {
    file.seek(std::io::SeekFrom::Start(offset)).await?;
    let mut data = vec![0u8; len];
    file.read_exact(&mut data).await?;
    Ok(data)
}

fn read_part_state(path: &Path) -> Option<PartState> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

fn persist_part_state(
    path: &Path,
    state: &mut PartState,
    bitmap: &crate::sync::Bitmap,
) -> Result<()> {
    state.set_bitmap(bitmap);
    let bytes = serde_json::to_vec(state)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

async fn move_to_trash(
    share_root: &Path,
    target: &Path,
    job: &Job,
    manager: &TaskManager,
    task_id: &str,
) -> Result<()> {
    let relative = target
        .strip_prefix(share_root)
        .map_err(|_| HarborError::InvalidPath)?;
    let stamp = std::fs::metadata(share_root)
        .ok()
        .map(|_| now_ms() / 1000)
        .unwrap_or(0);
    let trash_path = share_root
        .join(".harbor-trash")
        .join(stamp.to_string())
        .join(relative);
    if let Some(parent) = trash_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::rename(target, &trash_path)
        .map_err(|err| HarborError::Storage(format!("移入隔离区失败：{err}")))?;
    manager
        .record_trash(
            task_id,
            format!(
                "{}/.harbor-trash/{stamp}/{}",
                job.share_dir,
                relative.to_string_lossy().replace('\\', "/")
            ),
        )
        .await;
    Ok(())
}

fn unique_conflict_target(target: &Path, peer: &str) -> PathBuf {
    let parent = target.parent().unwrap_or_else(|| Path::new("."));
    let Some(name) = target.file_name().and_then(|name| name.to_str()) else {
        return target.to_path_buf();
    };
    let date = utc_date(now_ms());
    let base = conflict_name(name, peer, &date);
    let mut candidate = parent.join(&base);
    if !candidate.exists() {
        return candidate;
    }
    let (stem, extension) = match base.rfind('.') {
        Some(index) if index > 0 => (&base[..index], &base[index + 1..]),
        _ => (base.as_str(), ""),
    };
    for counter in 2..10_000 {
        let name = if extension.is_empty() {
            format!("{stem} ({counter})")
        } else {
            format!("{stem} ({counter}).{extension}")
        };
        candidate = parent.join(name);
        if !candidate.exists() {
            return candidate;
        }
    }
    target.to_path_buf()
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", bytes, UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}
