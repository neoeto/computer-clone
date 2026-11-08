//! 同步任务管理：持久化、状态机、进度聚合与事件节流。
//!
//! 任务记录保存在 `<data_dir>/tasks/<id>.json`，文件级实时状态只保留在内存中；
//! 断点数据保存在目标目录的 `.harbor/parts/<task_id>/`，两者通过任务 id 关联。
//! 应用重启后，运行中的任务会变为「已中断」，可从缺失块继续。

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{broadcast, Mutex, RwLock};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::chunker;
use crate::engine::Engine;
use crate::error::{HarborError, Result};
use crate::sync::{
    compute_percent, CreateTaskRequest, FileProgress, FileState, PartState, PersistedTask,
    TaskDetail, TaskFailure, TaskShare, TaskState, TaskSummary, TASK_STATE_VERSION,
};
use crate::types::CoreEvent;
use crate::util::{expand_tilde, now_ms};

const EMIT_INTERVAL: Duration = Duration::from_millis(240);
const SAVE_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StopReason {
    Pause,
    Cancel,
    Interrupt,
}

pub(crate) struct SpeedMeter {
    last_at: u64,
    last_bytes: u64,
    speed: f64,
}

impl SpeedMeter {
    pub(crate) fn new() -> Self {
        Self {
            last_at: now_ms(),
            last_bytes: 0,
            speed: 0.0,
        }
    }

    pub(crate) fn reset(&mut self, bytes: u64) {
        self.last_at = now_ms();
        self.last_bytes = bytes;
        self.speed = 0.0;
    }

    pub(crate) fn sample(&mut self, now: u64, bytes: u64) -> u64 {
        let elapsed = now.saturating_sub(self.last_at);
        if elapsed < 200 {
            return self.speed.max(0.0) as u64;
        }
        let delta = bytes.saturating_sub(self.last_bytes);
        let instant = delta as f64 * 1000.0 / elapsed as f64;
        if self.speed <= 0.0 {
            self.speed = instant;
        } else {
            self.speed = self.speed * 0.72 + instant * 0.28;
        }
        if delta == 0 {
            self.speed *= 0.85;
        }
        self.last_at = now;
        self.last_bytes = bytes;
        self.speed.max(0.0) as u64
    }
}

pub(crate) struct TaskEntry {
    pub(crate) persisted: PersistedTask,
    pub(crate) files: Vec<FileProgress>,
    pub(crate) speed: SpeedMeter,
    pub(crate) last_emit: Option<Instant>,
    pub(crate) last_save: Instant,
    pub(crate) stop_reason: Option<StopReason>,
    pub(crate) run_stamp: u64,
    pub(crate) resumed_bytes: u64,
    pub(crate) verified_bytes: u64,
    pub(crate) renamed: u64,
    pub(crate) trash: Vec<String>,
}

impl TaskEntry {
    fn new(persisted: PersistedTask) -> Self {
        Self {
            persisted,
            files: Vec::new(),
            speed: SpeedMeter::new(),
            last_emit: None,
            last_save: Instant::now(),
            stop_reason: None,
            run_stamp: now_ms() / 1000,
            resumed_bytes: 0,
            verified_bytes: 0,
            renamed: 0,
            trash: Vec::new(),
        }
    }

    pub(crate) fn summary(&self) -> TaskSummary {
        self.persisted.summary.clone()
    }
}

pub struct TaskManager {
    dir: PathBuf,
    entries: RwLock<HashMap<String, Arc<Mutex<TaskEntry>>>>,
    running: Mutex<HashMap<String, CancellationToken>>,
    save_lock: Mutex<()>,
    events: broadcast::Sender<CoreEvent>,
}

impl TaskManager {
    pub fn load(dir: PathBuf, events: broadcast::Sender<CoreEvent>) -> Result<Self> {
        fs::create_dir_all(&dir)
            .map_err(|err| HarborError::Storage(format!("创建任务目录失败：{err}")))?;
        let mut entries = HashMap::new();
        let read = match fs::read_dir(&dir) {
            Ok(read) => read,
            Err(err) => {
                tracing::warn!(?err, "读取任务目录失败");
                return Ok(Self {
                    dir,
                    entries: RwLock::new(entries),
                    running: Mutex::new(HashMap::new()),
                    save_lock: Mutex::new(()),
                    events,
                });
            }
        };

        for item in read.flatten() {
            let path = item.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let Ok(text) = fs::read_to_string(&path) else {
                continue;
            };
            let mut persisted: PersistedTask = match serde_json::from_str(&text) {
                Ok(value) => value,
                Err(err) => {
                    tracing::warn!(?path, ?err, "任务记录损坏，已忽略");
                    continue;
                }
            };
            if matches!(
                persisted.summary.state,
                TaskState::Scanning | TaskState::Running
            ) {
                persisted.summary.state = TaskState::Interrupted;
                persisted.summary.speed_bps = 0;
                persisted.summary.eta_secs = None;
                persisted.summary.message = "上次同步中断，可继续从断点恢复".into();
                persisted.summary.updated_at = now_ms();
            }
            entries.insert(
                persisted.summary.id.clone(),
                Arc::new(Mutex::new(TaskEntry::new(persisted))),
            );
        }

        Ok(Self {
            dir,
            entries: RwLock::new(entries),
            running: Mutex::new(HashMap::new()),
            save_lock: Mutex::new(()),
            events,
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub(crate) async fn entry(&self, id: &str) -> Result<Arc<Mutex<TaskEntry>>> {
        self.entries
            .read()
            .await
            .get(id)
            .cloned()
            .ok_or_else(|| HarborError::NotFound(id.to_string()))
    }

    pub async fn list(&self) -> Vec<TaskSummary> {
        let entries: Vec<_> = self.entries.read().await.values().cloned().collect();
        let mut out = Vec::with_capacity(entries.len());
        for entry in entries {
            out.push(entry.lock().await.summary());
        }
        out.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        out
    }

    pub async fn detail(&self, id: &str) -> Result<TaskDetail> {
        let entry = self.entry(id).await?;
        let guard = entry.lock().await;
        let files = select_detail_files(&guard.files);
        Ok(TaskDetail {
            summary: guard.summary(),
            files,
            failures: guard.persisted.failures.clone(),
            report: guard.persisted.report.clone(),
        })
    }

    pub(crate) async fn insert(&self, persisted: PersistedTask) -> Result<TaskSummary> {
        let id = persisted.summary.id.clone();
        let entry = Arc::new(Mutex::new(TaskEntry::new(persisted)));
        {
            let mut entries = self.entries.write().await;
            if entries.contains_key(&id) {
                return Err(HarborError::Conflict("任务已存在".into()));
            }
            entries.insert(id.clone(), entry);
        }
        self.save(&id).await?;
        let summary = self.entry(&id).await?.lock().await.summary();
        let _ = self.events.send(CoreEvent::TaskUpdated {
            task: summary.clone(),
        });
        Ok(summary)
    }

    pub(crate) async fn remove(&self, id: &str) -> Result<PersistedTask> {
        if self.running.lock().await.contains_key(id) {
            return Err(HarborError::Conflict("任务正在运行，请先取消".into()));
        }
        let entry = self
            .entries
            .write()
            .await
            .remove(id)
            .ok_or_else(|| HarborError::NotFound(id.to_string()))?;
        let persisted = entry.lock().await.persisted.clone();
        let _ = fs::remove_file(self.task_path(id));
        let _ = self.events.send(CoreEvent::TaskRemoved {
            task_id: id.to_string(),
        });
        Ok(persisted)
    }

    pub(crate) async fn begin_run(&self, id: &str) -> Result<CancellationToken> {
        let mut running = self.running.lock().await;
        if running.contains_key(id) {
            return Err(HarborError::Conflict("任务已经在运行".into()));
        }
        let token = CancellationToken::new();
        running.insert(id.to_string(), token.clone());
        Ok(token)
    }

    pub(crate) async fn end_run(&self, id: &str) {
        self.running.lock().await.remove(id);
    }

    pub(crate) async fn cancel_token(&self, id: &str) -> Option<CancellationToken> {
        self.running.lock().await.get(id).cloned()
    }

    pub(crate) fn events(&self) -> &broadcast::Sender<CoreEvent> {
        &self.events
    }

    pub(crate) async fn save(&self, id: &str) -> Result<()> {
        let path = self.task_path(id);
        let tmp = path.with_extension("json.tmp");
        // 在 save_lock 内重新取快照，避免较早的写任务覆盖较新的状态。
        let _guard = self.save_lock.lock().await;
        let entry = self.entry(id).await?;
        let persisted = entry.lock().await.persisted.clone();
        let bytes = serde_json::to_vec_pretty(&persisted)?;
        tokio::task::spawn_blocking(move || -> Result<()> {
            fs::write(&tmp, bytes)
                .map_err(|err| HarborError::Storage(format!("写入任务失败：{err}")))?;
            fs::rename(&tmp, &path)
                .map_err(|err| HarborError::Storage(format!("替换任务文件失败：{err}")))?;
            Ok(())
        })
        .await
        .map_err(|err| HarborError::Storage(format!("保存任务失败：{err}")))??;
        Ok(())
    }

    pub(crate) async fn save_if_due(&self, id: &str) {
        let due = match self.entry(id).await {
            Ok(entry) => {
                let mut guard = entry.lock().await;
                if guard.last_save.elapsed() >= SAVE_INTERVAL {
                    guard.last_save = Instant::now();
                    true
                } else {
                    false
                }
            }
            Err(_) => false,
        };
        if due {
            if let Err(err) = self.save(id).await {
                tracing::warn!(?err, "任务保存失败");
            }
        }
    }

    /// 刷新速度/百分比并按 240 ms 节流推送进度事件。
    pub(crate) async fn publish(&self, id: &str, force: bool) {
        let payload = match self.entry(id).await {
            Ok(entry) => {
                let mut guard = entry.lock().await;
                let now = now_ms();
                let done_bytes = guard.persisted.summary.done_bytes;
                let speed = guard.speed.sample(now, done_bytes);
                let files = select_active_files(&guard.files);
                {
                    let summary = &mut guard.persisted.summary;
                    summary.speed_bps = speed;
                    summary.percent = compute_percent(
                        summary.done_bytes,
                        summary.total_bytes,
                        summary.done_files,
                        summary.skipped_files,
                        summary.failed_files,
                        summary.total_files,
                    );
                    summary.eta_secs = if speed > 0 && summary.total_bytes > summary.done_bytes {
                        Some((summary.total_bytes - summary.done_bytes) / speed)
                    } else if summary.state == TaskState::Done {
                        Some(0)
                    } else {
                        None
                    };
                    summary.updated_at = now;
                }
                let due = force
                    || guard
                        .last_emit
                        .map(|last| last.elapsed() >= EMIT_INTERVAL)
                        .unwrap_or(true);
                if !due {
                    None
                } else {
                    guard.last_emit = Some(Instant::now());
                    Some((guard.persisted.summary.clone(), files))
                }
            }
            Err(_) => None,
        };

        if let Some((task, files)) = payload {
            let _ = self.events.send(CoreEvent::TaskProgress { task, files });
        }
    }

    pub(crate) async fn publish_state(&self, id: &str) {
        let summary = match self.entry(id).await {
            Ok(entry) => {
                let mut guard = entry.lock().await;
                guard.persisted.summary.updated_at = now_ms();
                guard.persisted.summary.refresh_progress();
                guard.summary()
            }
            Err(_) => return,
        };
        let _ = self.events.send(CoreEvent::TaskUpdated { task: summary });
    }

    pub(crate) async fn set_state(&self, id: &str, state: TaskState, message: impl Into<String>) {
        if let Ok(entry) = self.entry(id).await {
            let mut guard = entry.lock().await;
            guard.persisted.summary.state = state;
            guard.persisted.summary.message = message.into();
            guard.persisted.summary.updated_at = now_ms();
            guard.persisted.summary.refresh_progress();
        }
    }

    pub(crate) async fn pause(&self, id: &str) -> Result<TaskSummary> {
        let entry = self.entry(id).await?;
        let token = self.cancel_token(id).await;
        let summary = {
            let mut guard = entry.lock().await;
            if !guard.persisted.summary.state.is_active() {
                return Err(HarborError::Conflict("任务当前不在运行".into()));
            }
            guard.stop_reason = Some(StopReason::Pause);
            guard.persisted.summary.state = TaskState::Paused;
            guard.persisted.summary.message = format!(
                "已暂停 · 已传输 {}，可继续从断点恢复",
                human_bytes(guard.persisted.summary.done_bytes)
            );
            guard.persisted.summary.speed_bps = 0;
            guard.persisted.summary.eta_secs = None;
            guard.persisted.summary.updated_at = now_ms();
            guard.persisted.summary.refresh_progress();
            guard.summary()
        };
        if let Some(token) = token {
            token.cancel();
        }
        self.save(id).await?;
        self.publish_state(id).await;
        Ok(summary)
    }

    pub(crate) async fn cancel(&self, id: &str) -> Result<TaskSummary> {
        let entry = self.entry(id).await?;
        let token = self.cancel_token(id).await;
        let summary = {
            let mut guard = entry.lock().await;
            if !guard.persisted.summary.state.is_active() {
                return Err(HarborError::Conflict("任务当前不在运行".into()));
            }
            guard.stop_reason = Some(StopReason::Cancel);
            guard.persisted.summary.state = TaskState::Cancelled;
            guard.persisted.summary.finished_at = Some(now_ms());
            guard.persisted.summary.message = "已取消，断点数据已保留".into();
            guard.persisted.summary.speed_bps = 0;
            guard.persisted.summary.eta_secs = None;
            guard.persisted.summary.updated_at = now_ms();
            guard.persisted.summary.refresh_progress();
            guard.summary()
        };
        if let Some(token) = token {
            token.cancel();
        }
        self.save(id).await?;
        self.publish_state(id).await;
        Ok(summary)
    }

    pub(crate) async fn interrupt_session(&self, session_id: &str, message: &str) {
        let entries: Vec<_> = self.entries.read().await.values().cloned().collect();
        for entry in entries {
            let id = {
                let mut guard = entry.lock().await;
                if guard.persisted.summary.session_id != session_id
                    || !guard.persisted.summary.state.is_active()
                {
                    continue;
                }
                guard.stop_reason = Some(StopReason::Interrupt);
                guard.persisted.summary.state = TaskState::Interrupted;
                guard.persisted.summary.message = message.to_string();
                guard.persisted.summary.speed_bps = 0;
                guard.persisted.summary.eta_secs = None;
                guard.persisted.summary.updated_at = now_ms();
                guard.summary().id
            };
            if let Some(token) = self.cancel_token(&id).await {
                token.cancel();
            }
            let _ = self.save(&id).await;
            self.publish_state(&id).await;
        }
    }

    fn task_path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.json"))
    }
}

/* ------------------------------------------------------------------ */
/* 进度与文件状态辅助                                                  */
/* ------------------------------------------------------------------ */

fn select_active_files(files: &[FileProgress]) -> Vec<FileProgress> {
    let mut out = Vec::new();
    for file in files {
        if matches!(file.state, FileState::Transferring | FileState::Verifying) {
            out.push(file.clone());
            if out.len() >= 8 {
                return out;
            }
        }
    }
    for file in files {
        if matches!(file.state, FileState::Failed) {
            out.push(file.clone());
            if out.len() >= 8 {
                return out;
            }
        }
    }
    for file in files {
        if matches!(file.state, FileState::Queued) {
            out.push(file.clone());
            if out.len() >= 8 {
                return out;
            }
        }
    }
    if out.is_empty() {
        out.extend(files.iter().rev().take(5).cloned());
    }
    out
}

fn select_detail_files(files: &[FileProgress]) -> Vec<FileProgress> {
    if files.len() <= 400 {
        return files.to_vec();
    }
    let mut chosen = HashSet::new();
    for (index, file) in files.iter().enumerate() {
        if matches!(
            file.state,
            FileState::Transferring | FileState::Verifying | FileState::Failed
        ) {
            chosen.insert(index);
            if chosen.len() >= 240 {
                break;
            }
        }
    }
    for (index, file) in files.iter().enumerate() {
        if file.state == FileState::Queued {
            chosen.insert(index);
            if chosen.len() >= 360 {
                break;
            }
        }
    }
    for (index, file) in files.iter().enumerate().rev() {
        if matches!(
            file.state,
            FileState::Done | FileState::Skipped | FileState::Conflict
        ) {
            chosen.insert(index);
            if chosen.len() >= 400 {
                break;
            }
        }
    }
    let mut indexes: Vec<usize> = chosen.into_iter().collect();
    indexes.sort_unstable();
    indexes
        .into_iter()
        .map(|index| files[index].clone())
        .collect()
}

/// 状态迁移同时维护计数器；避免每个文件都全量重算。
pub(crate) fn set_file_state(
    entry: &mut TaskEntry,
    index: usize,
    state: FileState,
    error: Option<String>,
) {
    let Some(file) = entry.files.get(index) else {
        return;
    };
    let old = file.state;
    if old == state {
        entry.files[index].error = error;
        return;
    }
    let summary = &mut entry.persisted.summary;
    match old {
        FileState::Done => summary.done_files = summary.done_files.saturating_sub(1),
        FileState::Skipped => summary.skipped_files = summary.skipped_files.saturating_sub(1),
        FileState::Failed => summary.failed_files = summary.failed_files.saturating_sub(1),
        FileState::Conflict => summary.conflict_files = summary.conflict_files.saturating_sub(1),
        _ => {}
    }
    if matches!(
        state,
        FileState::Done | FileState::Skipped | FileState::Conflict
    ) {
        let file = &mut entry.files[index];
        if file.done_bytes < file.size {
            let delta = file.size - file.done_bytes;
            file.done_bytes = file.size;
            summary.done_bytes = summary.done_bytes.saturating_add(delta);
        }
    }
    match state {
        FileState::Done => summary.done_files += 1,
        FileState::Skipped => summary.skipped_files += 1,
        FileState::Failed => summary.failed_files += 1,
        FileState::Conflict => summary.conflict_files += 1,
        _ => {}
    }
    let file = &mut entry.files[index];
    file.state = state;
    file.error = error;
}

/// 记录文件已落盘字节；只增加不重复计数。
pub(crate) fn add_file_bytes(entry: &mut TaskEntry, index: usize, absolute_done: u64) {
    let Some(file) = entry.files.get_mut(index) else {
        return;
    };
    let next = absolute_done.min(file.size);
    if next > file.done_bytes {
        let delta = next - file.done_bytes;
        file.done_bytes = next;
        entry.persisted.summary.done_bytes =
            entry.persisted.summary.done_bytes.saturating_add(delta);
    }
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

pub(crate) fn record_failure(entry: &mut TaskEntry, index: usize, message: String) {
    let path = entry
        .files
        .get(index)
        .map(|file| file.path.clone())
        .unwrap_or_default();
    let share_id = entry
        .files
        .get(index)
        .map(|file| file.share_id.clone())
        .unwrap_or_default();
    set_file_state(entry, index, FileState::Failed, Some(message.clone()));
    entry.persisted.failures.push(TaskFailure {
        share_id,
        path,
        message,
        at: now_ms(),
    });
}

/* ------------------------------------------------------------------ */
/* 对外 API（挂在 Engine 上）                                          */
/* ------------------------------------------------------------------ */

impl Engine {
    pub async fn create_sync_task(
        self: &Arc<Self>,
        request: CreateTaskRequest,
    ) -> Result<TaskDetail> {
        let info = self.session_info(&request.session_id).await?;
        let remote_shares = self.remote_list_shares(&request.session_id).await?;
        let mut share_ids = request.share_ids.clone();
        share_ids.sort();
        share_ids.dedup();
        if share_ids.is_empty() {
            return Err(HarborError::Conflict("请至少选择一个共享文件夹".into()));
        }

        let mut shares = Vec::new();
        for id in &share_ids {
            let remote = remote_shares
                .iter()
                .find(|share| &share.id == id)
                .ok_or_else(|| HarborError::NotFound(id.clone()))?;
            shares.push(TaskShare {
                id: remote.id.clone(),
                name: remote.name.clone(),
            });
        }

        let dest_root = expand_tilde(&request.dest_root);
        fs::create_dir_all(&dest_root)
            .map_err(|err| HarborError::Storage(format!("创建保存位置失败：{err}")))?;
        let dest_root = dest_root
            .canonicalize()
            .map_err(|err| HarborError::Storage(format!("保存位置不可用：{err}")))?;
        if !dest_root.is_dir() {
            return Err(HarborError::InvalidPath);
        }

        let total_files: u64 = shares
            .iter()
            .filter_map(|share| remote_shares.iter().find(|remote| remote.id == share.id))
            .map(|share| share.file_count)
            .sum();
        let total_bytes: u64 = shares
            .iter()
            .filter_map(|share| remote_shares.iter().find(|remote| remote.id == share.id))
            .map(|share| share.total_bytes)
            .sum();

        let now = now_ms();
        let id = Uuid::new_v4().to_string();
        let summary = TaskSummary {
            id: id.clone(),
            session_id: request.session_id.clone(),
            peer_name: info.device.name.clone(),
            peer_device_id: info.device.id.clone(),
            dest_root: dest_root.to_string_lossy().to_string(),
            shares: shares.iter().map(|share| share.name.clone()).collect(),
            mode: request.mode,
            conflict: request.conflict,
            verify_strict: request.verify_strict,
            state: TaskState::Pending,
            total_files,
            done_files: 0,
            skipped_files: 0,
            failed_files: 0,
            conflict_files: 0,
            total_bytes,
            done_bytes: 0,
            speed_bps: 0,
            eta_secs: None,
            percent: 0.0,
            message: "等待开始".into(),
            created_at: now,
            updated_at: now,
            started_at: None,
            finished_at: None,
        };
        let persisted = PersistedTask {
            version: TASK_STATE_VERSION,
            summary,
            shares,
            peer_address: info.address.clone(),
            peer_port: info.port,
            failures: Vec::new(),
            report: None,
        };
        self.tasks.insert(persisted).await?;
        self.tasks.detail(&id).await
    }

    pub async fn start_sync_task(self: &Arc<Self>, task_id: &str) -> Result<TaskSummary> {
        let entry = self.tasks.entry(task_id).await?;
        let snapshot = {
            let guard = entry.lock().await;
            let summary = guard.persisted.summary.clone();
            if matches!(summary.state, TaskState::Scanning | TaskState::Running) {
                return Ok(guard.summary());
            }
            if !summary.state.is_resumable() {
                return Err(HarborError::Conflict("该任务当前无法开始".into()));
            }
            guard.summary()
        };

        // 重连后会话 id 会变化：先按原会话查找，再按设备 id 找回。
        let mut session_id = snapshot.session_id.clone();
        let mut info = self.session_info(&session_id).await.ok();
        if info
            .as_ref()
            .map(|info| info.device.id != snapshot.peer_device_id)
            .unwrap_or(true)
        {
            let found = self.find_session_for_peer(&snapshot.peer_device_id).await;
            let Some((new_id, new_info)) = found else {
                return Err(HarborError::Other(
                    "请先连接源端，再从断点继续这个任务".into(),
                ));
            };
            session_id = new_id;
            info = Some(new_info);
        }
        let info = info.ok_or_else(|| HarborError::Other("源端连接已断开".into()))?;

        let token = self.tasks.begin_run(task_id).await?;
        let now = now_ms();
        {
            let mut guard = entry.lock().await;
            let summary = &mut guard.persisted.summary;
            summary.session_id = session_id;
            summary.peer_name = info.device.name.clone();
            summary.peer_device_id = info.device.id.clone();
            summary.state = TaskState::Scanning;
            summary.started_at = Some(now);
            summary.finished_at = None;
            summary.done_files = 0;
            summary.skipped_files = 0;
            summary.failed_files = 0;
            summary.conflict_files = 0;
            summary.done_bytes = 0;
            summary.speed_bps = 0;
            summary.eta_secs = None;
            summary.percent = 0.0;
            summary.message = "正在扫描源端清单…".into();
            summary.updated_at = now;
            guard.files.clear();
            guard.persisted.failures.clear();
            guard.persisted.report = None;
            guard.stop_reason = None;
            guard.speed = SpeedMeter::new();
            guard.resumed_bytes = 0;
            guard.verified_bytes = 0;
            guard.renamed = 0;
            guard.trash.clear();
            guard.run_stamp = now / 1000;
        }
        if let Err(err) = self.tasks.save(task_id).await {
            self.tasks.end_run(task_id).await;
            self.tasks
                .set_state(task_id, TaskState::Interrupted, "任务启动失败，请重试")
                .await;
            return Err(err);
        }
        self.tasks.publish_state(task_id).await;

        let engine = self.clone();
        let id = task_id.to_string();
        tokio::spawn(async move {
            crate::receive::run_task(engine, id, token).await;
        });

        let summary = entry.lock().await.summary();
        Ok(summary)
    }

    pub async fn pause_sync_task(&self, task_id: &str) -> Result<TaskSummary> {
        self.tasks.pause(task_id).await
    }

    pub async fn cancel_sync_task(&self, task_id: &str) -> Result<TaskSummary> {
        self.tasks.cancel(task_id).await
    }

    pub async fn retry_sync_task(self: &Arc<Self>, task_id: &str) -> Result<TaskSummary> {
        let summary = self.tasks.entry(task_id).await?.lock().await.summary();
        if matches!(
            summary.state,
            TaskState::Done | TaskState::Failed | TaskState::Cancelled
        ) {
            // 重试前把状态重置为待恢复，复用同一套断点。
            self.tasks
                .set_state(task_id, TaskState::Interrupted, "准备重试失败项")
                .await;
            self.tasks.save(task_id).await?;
        }
        self.start_sync_task(task_id).await
    }

    pub async fn remove_sync_task(&self, task_id: &str) -> Result<()> {
        let persisted = self.tasks.remove(task_id).await?;
        let dest_root = PathBuf::from(&persisted.summary.dest_root);
        let share_dirs = crate::sync::share_directory_names(&persisted.shares);
        for share_dir in &share_dirs {
            let part_dir = dest_root
                .join(share_dir)
                .join(".harbor")
                .join("parts")
                .join(task_id);
            if part_dir.exists() {
                let _ = fs::remove_dir_all(part_dir);
            }
        }
        Ok(())
    }

    pub async fn list_sync_tasks(&self) -> Vec<TaskSummary> {
        self.tasks.list().await
    }

    pub async fn get_sync_task(&self, task_id: &str) -> Result<TaskDetail> {
        self.tasks.detail(task_id).await
    }

    /// 校验任务断点目录，返回 (已收到字节, 文件数)。用于 UI 展示「已恢复」。
    pub fn inspect_parts(
        &self,
        task_id: &str,
        dest_root: &str,
        shares: &[TaskShare],
    ) -> (u64, usize) {
        let root = PathBuf::from(dest_root);
        let mut bytes = 0u64;
        let mut count = 0usize;
        for share_dir in crate::sync::share_directory_names(shares) {
            let dir = root
                .join(share_dir)
                .join(".harbor")
                .join("parts")
                .join(task_id);
            let Ok(read) = fs::read_dir(&dir) else {
                continue;
            };
            for item in read.flatten() {
                if item.path().extension().and_then(|ext| ext.to_str()) != Some("json") {
                    continue;
                }
                if let Ok(text) = fs::read_to_string(item.path()) {
                    if let Ok(state) = serde_json::from_str::<PartState>(&text) {
                        if let Ok(meta) = fs::metadata(item.path().with_extension("part")) {
                            if meta.len() == state.size {
                                let bitmap = state.bitmap();
                                if bitmap.count() > 0 {
                                    count += 1;
                                    bytes += state
                                        .chunk_lens
                                        .iter()
                                        .enumerate()
                                        .filter(|(index, _)| bitmap.get(*index as u32))
                                        .map(|(_, len)| *len as u64)
                                        .sum::<u64>();
                                }
                            }
                        }
                    }
                }
            }
        }
        (bytes, count)
    }
}

/// 计算分块时使用的稳定文件键。
pub(crate) fn part_key(share_id: &str, path: &str) -> String {
    let mut input = Vec::with_capacity(share_id.len() + path.len() + 1);
    input.extend_from_slice(share_id.as_bytes());
    input.push(0);
    input.extend_from_slice(path.as_bytes());
    let hash = chunker::chunk_hash(&input);
    hex::encode(&hash[..16])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::{
        ChunkInfo, ConflictPolicy, FileProgress, RemoteFileHeader, SyncMode, TaskSummary,
    };

    fn summary() -> TaskSummary {
        TaskSummary {
            id: "t1".into(),
            session_id: "s1".into(),
            peer_name: "peer".into(),
            peer_device_id: "p1".into(),
            dest_root: "/tmp".into(),
            shares: vec!["share".into()],
            mode: SyncMode::Delta,
            conflict: ConflictPolicy::Overwrite,
            verify_strict: false,
            state: TaskState::Running,
            total_files: 2,
            done_files: 0,
            skipped_files: 0,
            failed_files: 0,
            conflict_files: 0,
            total_bytes: 20,
            done_bytes: 0,
            speed_bps: 0,
            eta_secs: None,
            percent: 0.0,
            message: String::new(),
            created_at: 0,
            updated_at: 0,
            started_at: None,
            finished_at: None,
        }
    }

    fn file(path: &str, size: u64) -> FileProgress {
        FileProgress {
            key: path.into(),
            share_id: "share".into(),
            path: path.into(),
            dest_path: path.into(),
            size,
            done_bytes: 0,
            state: FileState::Queued,
            error: None,
            resumed: false,
        }
    }

    #[test]
    fn state_transitions_keep_counters_consistent() {
        let persisted = PersistedTask {
            version: TASK_STATE_VERSION,
            summary: summary(),
            shares: vec![],
            peer_address: String::new(),
            peer_port: 0,
            failures: vec![],
            report: None,
        };
        let mut entry = TaskEntry::new(persisted);
        entry.files = vec![file("a", 10), file("b", 10)];

        set_file_state(&mut entry, 0, FileState::Skipped, None);
        assert_eq!(entry.persisted.summary.skipped_files, 1);
        assert_eq!(entry.persisted.summary.done_bytes, 10);

        add_file_bytes(&mut entry, 1, 4);
        assert_eq!(entry.persisted.summary.done_bytes, 14);
        set_file_state(&mut entry, 1, FileState::Done, None);
        assert_eq!(entry.persisted.summary.done_files, 1);
        assert_eq!(entry.persisted.summary.done_bytes, 20);

        record_failure(&mut entry, 0, "boom".into());
        assert_eq!(entry.persisted.summary.skipped_files, 0);
        assert_eq!(entry.persisted.summary.failed_files, 1);
        assert_eq!(entry.persisted.failures.len(), 1);
    }

    #[test]
    fn part_key_is_stable() {
        assert_eq!(part_key("share", "a/b.txt"), part_key("share", "a/b.txt"));
        assert_ne!(part_key("share", "a/b.txt"), part_key("share", "a/c.txt"));
        assert_eq!(part_key("share", "a/b.txt").len(), 32);
    }

    #[test]
    fn speed_meter_is_monotonic_on_progress() {
        let mut meter = SpeedMeter::new();
        meter.sample(now_ms(), 0);
        let speed = meter.sample(now_ms() + 1000, 5 * 1024 * 1024);
        assert!(speed > 0);
    }

    #[test]
    fn header_matches_part_state() {
        let header = RemoteFileHeader {
            size: 3,
            mtime_ms: Some(1),
            mode: 0,
            content_hash: hex::encode([9u8; 32]),
            chunks: vec![
                ChunkInfo {
                    len: 2,
                    hash: hex::encode([1u8; 32]),
                },
                ChunkInfo {
                    len: 1,
                    hash: hex::encode([2u8; 32]),
                },
            ],
            stable: true,
        };
        let state = PartState::new(&header);
        assert!(state.matches(&header));
    }
}
