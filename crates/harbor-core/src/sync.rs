//! M2 同步任务模型、断点状态与跨平台路径处理。
//!
//! 任务状态同时保存在磁盘（`<data_dir>/tasks/<id>.json`）与内存中，
//! 应用重启后未完成任务会标记为「已中断」，可一键继续。断点数据按文件
//! 保存在目标目录的 `.harbor/parts/<task>/` 下，与任务记录解耦。

use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::chunker;
use crate::error::{HarborError, Result};
use crate::util::now_ms;

pub const TASK_STATE_VERSION: u32 = 1;
pub const MANIFEST_PAGE_SIZE: usize = 1000;
pub const CHUNK_META_PAGE_SIZE: u32 = 4096;

/* ------------------------------------------------------------------ */
/* 源端清单                                                            */
/* ------------------------------------------------------------------ */

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestEntry {
    /// 相对共享根的路径，统一使用 `/`。
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    pub mtime_ms: Option<u64>,
    /// Unix 权限位；Windows 上为 0。
    #[serde(default)]
    pub mode: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestPage {
    pub share_id: String,
    pub entries: Vec<ManifestEntry>,
    pub next: Option<String>,
    pub total_files: u64,
    pub total_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkInfo {
    pub len: u32,
    /// 十六进制 BLAKE3。
    pub hash: String,
}

impl ChunkInfo {
    pub fn from_chunk(chunk: &chunker::Chunk) -> Self {
        Self {
            len: chunk.len,
            hash: hex::encode(chunk.hash),
        }
    }

    pub fn hash_bytes(&self) -> Result<[u8; 32]> {
        let raw = hex::decode(&self.hash)
            .map_err(|_| HarborError::Protocol("块哈希不是合法十六进制".into()))?;
        raw.try_into()
            .map_err(|_| HarborError::Protocol("块哈希长度不正确".into()))
    }
}

/// 数据连接返回的文件头：源端已完成分块与内容哈希。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteFileHeader {
    pub size: u64,
    pub mtime_ms: Option<u64>,
    pub mode: u32,
    pub content_hash: String,
    pub chunks: Vec<ChunkInfo>,
    /// 源端在分块后再次确认文件未变化。
    pub stable: bool,
}

impl RemoteFileHeader {
    pub fn chunk_lens(&self) -> Vec<u32> {
        self.chunks.iter().map(|chunk| chunk.len).collect()
    }

    pub fn content_hash_bytes(&self) -> Result<[u8; 32]> {
        let raw = hex::decode(&self.content_hash)
            .map_err(|_| HarborError::Protocol("文件哈希不是合法十六进制".into()))?;
        raw.try_into()
            .map_err(|_| HarborError::Protocol("文件哈希长度不正确".into()))
    }
}

/* ------------------------------------------------------------------ */
/* 任务模型                                                            */
/* ------------------------------------------------------------------ */

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SyncMode {
    /// 增量：传输新增/变化文件，保留本地多余文件。
    Delta,
    /// 镜像：让目标目录与源端一致，多余文件移入隔离区。
    Mirror,
    /// 仅新增：只补本地缺失文件。
    NewOnly,
}

impl Default for SyncMode {
    fn default() -> Self {
        Self::Delta
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConflictPolicy {
    Overwrite,
    Skip,
    KeepBoth,
}

impl Default for ConflictPolicy {
    fn default() -> Self {
        Self::Overwrite
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    Pending,
    Scanning,
    Running,
    Paused,
    Interrupted,
    Done,
    Failed,
    Cancelled,
}

impl TaskState {
    pub fn is_active(self) -> bool {
        matches!(
            self,
            TaskState::Pending | TaskState::Scanning | TaskState::Running
        )
    }

    pub fn is_resumable(self) -> bool {
        matches!(
            self,
            TaskState::Pending
                | TaskState::Paused
                | TaskState::Interrupted
                | TaskState::Done
                | TaskState::Failed
        )
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FileState {
    Queued,
    Transferring,
    Verifying,
    Done,
    Skipped,
    Conflict,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileProgress {
    /// 任务内唯一键：`share_id + \u{1f} + 源端相对路径`。
    pub key: String,
    pub share_id: String,
    /// 源端相对路径。
    pub path: String,
    /// 目标根下的相对路径（可能因文件名清洗或冲突改名而不同）。
    pub dest_path: String,
    pub size: u64,
    pub done_bytes: u64,
    pub state: FileState,
    pub error: Option<String>,
    pub resumed: bool,
}

impl FileProgress {
    pub fn percent(&self) -> f64 {
        if self.size == 0 {
            return if matches!(self.state, FileState::Done | FileState::Skipped) {
                1.0
            } else {
                0.0
            };
        }
        (self.done_bytes as f64 / self.size as f64).clamp(0.0, 1.0)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskShare {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskFailure {
    pub share_id: String,
    pub path: String,
    pub message: String,
    pub at: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskReport {
    pub duration_ms: u64,
    pub average_bps: u64,
    pub transferred_files: u64,
    pub skipped_files: u64,
    pub failed_files: u64,
    pub conflicts: u64,
    pub resumed_bytes: u64,
    pub verified_bytes: u64,
    pub renamed: u64,
    pub trash: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSummary {
    pub id: String,
    pub session_id: String,
    pub peer_name: String,
    pub peer_device_id: String,
    pub dest_root: String,
    pub shares: Vec<String>,
    pub mode: SyncMode,
    pub conflict: ConflictPolicy,
    pub verify_strict: bool,
    pub state: TaskState,
    pub total_files: u64,
    pub done_files: u64,
    pub skipped_files: u64,
    pub failed_files: u64,
    pub conflict_files: u64,
    pub total_bytes: u64,
    pub done_bytes: u64,
    pub speed_bps: u64,
    pub eta_secs: Option<u64>,
    pub percent: f64,
    pub message: String,
    pub created_at: u64,
    pub updated_at: u64,
    pub started_at: Option<u64>,
    pub finished_at: Option<u64>,
}

impl TaskSummary {
    pub fn refresh_progress(&mut self) {
        self.percent = compute_percent(
            self.done_bytes,
            self.total_bytes,
            self.done_files,
            self.skipped_files,
            self.failed_files,
            self.total_files,
        );
        if let (Some(started), Some(finished)) = (self.started_at, self.finished_at) {
            let elapsed = finished.saturating_sub(started);
            self.eta_secs = Some(0);
            if elapsed > 0 {
                self.speed_bps = self
                    .done_bytes
                    .saturating_mul(1000)
                    .checked_div(elapsed)
                    .unwrap_or(0);
            }
        }
    }
}

pub fn compute_percent(
    done_bytes: u64,
    total_bytes: u64,
    done_files: u64,
    skipped_files: u64,
    failed_files: u64,
    total_files: u64,
) -> f64 {
    if total_files > 0 && (failed_files > 0 || total_bytes == 0) {
        return ((done_files + skipped_files) as f64 / total_files as f64).clamp(0.0, 1.0);
    }
    if total_bytes > 0 {
        (done_bytes as f64 / total_bytes as f64).clamp(0.0, 1.0)
    } else {
        0.0
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskDetail {
    pub summary: TaskSummary,
    pub files: Vec<FileProgress>,
    pub failures: Vec<TaskFailure>,
    pub report: Option<TaskReport>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateTaskRequest {
    pub session_id: String,
    #[serde(default)]
    pub share_ids: Vec<String>,
    pub dest_root: String,
    #[serde(default)]
    pub mode: SyncMode,
    #[serde(default)]
    pub conflict: ConflictPolicy,
    #[serde(default)]
    pub verify_strict: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedTask {
    pub version: u32,
    pub summary: TaskSummary,
    pub shares: Vec<TaskShare>,
    pub peer_address: String,
    pub peer_port: u16,
    pub failures: Vec<TaskFailure>,
    pub report: Option<TaskReport>,
}

/* ------------------------------------------------------------------ */
/* 断点状态                                                            */
/* ------------------------------------------------------------------ */

/// 已校验块位图。1 万块约 1.3 KB，和设计文档一致。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bitmap {
    bytes: Vec<u8>,
    len: u32,
}

impl Bitmap {
    pub fn new(len: u32) -> Self {
        Self {
            bytes: vec![0; (len as usize).div_ceil(8)],
            len,
        }
    }

    pub fn from_bytes(bytes: Vec<u8>, len: u32) -> Self {
        let expected = (len as usize).div_ceil(8);
        let mut bytes = bytes;
        bytes.resize(expected, 0);
        bytes.truncate(expected);
        Self { bytes, len }
    }

    pub fn len(&self) -> u32 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn get(&self, index: u32) -> bool {
        if index >= self.len {
            return false;
        }
        let byte = self.bytes[index as usize / 8];
        byte & (1 << (index % 8)) != 0
    }

    pub fn set(&mut self, index: u32) {
        if index >= self.len {
            return;
        }
        self.bytes[index as usize / 8] |= 1 << (index % 8);
    }

    pub fn clear(&mut self, index: u32) {
        if index >= self.len {
            return;
        }
        self.bytes[index as usize / 8] &= !(1 << (index % 8));
    }

    pub fn count(&self) -> u32 {
        self.bytes.iter().map(|byte| byte.count_ones()).sum()
    }

    pub fn is_full(&self) -> bool {
        self.count() == self.len
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn missing(&self) -> Vec<u32> {
        (0..self.len).filter(|index| !self.get(*index)).collect()
    }

    /// 合并另一个位图；长度不同则忽略越界部分。
    pub fn merge(&mut self, other: &Bitmap) {
        for index in 0..self.len.min(other.len) {
            if other.get(index) {
                self.set(index);
            }
        }
    }
}

/// 每个文件一份断点状态，与 `.part` 文件放在一起。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PartState {
    pub version: u32,
    pub size: u64,
    pub mtime_ms: Option<u64>,
    pub content_hash: String,
    pub chunk_lens: Vec<u32>,
    pub received: Vec<u8>,
    pub updated_at: u64,
}

impl PartState {
    pub fn new(header: &RemoteFileHeader) -> Self {
        Self {
            version: TASK_STATE_VERSION,
            size: header.size,
            mtime_ms: header.mtime_ms,
            content_hash: header.content_hash.clone(),
            chunk_lens: header.chunk_lens(),
            received: vec![0; header.chunks.len().div_ceil(8)],
            updated_at: now_ms(),
        }
    }

    pub fn matches(&self, header: &RemoteFileHeader) -> bool {
        self.version == TASK_STATE_VERSION
            && self.size == header.size
            && self.content_hash == header.content_hash
            && self.chunk_lens == header.chunk_lens()
    }

    pub fn bitmap(&self) -> Bitmap {
        Bitmap::from_bytes(
            self.received.clone(),
            self.chunk_lens.len().try_into().unwrap_or(u32::MAX),
        )
    }

    pub fn set_bitmap(&mut self, bitmap: &Bitmap) {
        self.received = bitmap.as_bytes().to_vec();
        self.updated_at = now_ms();
    }

    pub fn received_bytes(&self, chunks: &[ChunkInfo]) -> u64 {
        let bitmap = self.bitmap();
        chunks
            .iter()
            .enumerate()
            .filter(|(index, _)| bitmap.get(*index as u32))
            .map(|(_, chunk)| chunk.len as u64)
            .sum()
    }

    pub fn missing(&self) -> Vec<u32> {
        self.bitmap().missing()
    }
}

pub fn chunk_offsets(chunks: &[ChunkInfo]) -> Vec<u64> {
    let mut out = Vec::with_capacity(chunks.len());
    let mut offset = 0u64;
    for chunk in chunks {
        out.push(offset);
        offset += chunk.len as u64;
    }
    out
}

/* ------------------------------------------------------------------ */
/* 路径与文件名                                                        */
/* ------------------------------------------------------------------ */

/// Windows 非法字符与保留名清洗。即使运行在 macOS/Linux，也导出纯函数便于测试，
/// 真正落盘时只在 Windows 上应用。
pub fn sanitize_windows_component(name: &str) -> String {
    let mut out: String = name
        .chars()
        .map(|ch| {
            if ch.is_control() || matches!(ch, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*')
            {
                '_'
            } else {
                ch
            }
        })
        .collect();

    while out.ends_with(' ') || out.ends_with('.') {
        out.pop();
    }
    if out.is_empty() {
        out.push_str("unnamed");
    }

    let stem = out.split('.').next().unwrap_or("").to_ascii_uppercase();
    let reserved = matches!(
        stem.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    );
    if reserved {
        out = format!("_{out}");
    }
    out
}

fn sanitize_component(name: &str) -> Result<String> {
    if name.is_empty() || name == "." || name == ".." || name.contains('\0') {
        return Err(HarborError::InvalidPath);
    }
    if name.contains('/') || name.contains('\\') {
        return Err(HarborError::InvalidPath);
    }
    if name.len() > 255 {
        return Err(HarborError::InvalidPath);
    }
    let clean = if cfg!(windows) {
        sanitize_windows_component(name)
    } else {
        name.to_string()
    };
    if clean.is_empty() || clean == "." || clean == ".." {
        return Err(HarborError::InvalidPath);
    }
    Ok(clean)
}

/// 把源端相对路径拆成安全的路径组件，并清洗目标平台不允许的字符。
pub fn safe_rel_components(rel: &str) -> Result<Vec<String>> {
    if rel.is_empty() || rel.starts_with('/') || rel.starts_with('\\') || rel.contains('\0') {
        return Err(HarborError::InvalidPath);
    }
    if rel.len() > 4096 {
        return Err(HarborError::InvalidPath);
    }
    let mut parts = Vec::new();
    for raw in rel.split(['/', '\\']) {
        if raw.is_empty() || raw == "." {
            continue;
        }
        if raw == ".." {
            return Err(HarborError::InvalidPath);
        }
        parts.push(sanitize_component(raw)?);
    }
    if parts.is_empty() {
        return Err(HarborError::InvalidPath);
    }
    Ok(parts)
}

pub fn rel_path_from_components(parts: &[String]) -> String {
    parts.join("/")
}

pub fn join_components(root: &Path, parts: &[String]) -> PathBuf {
    let mut path = root.to_path_buf();
    for part in parts {
        path.push(part);
    }
    path
}

/// 清洗共享文件夹名称，用作目标根下的第一层目录。
pub fn safe_share_dir_name(name: &str) -> String {
    let cleaned = if cfg!(windows) {
        sanitize_windows_component(name)
    } else {
        let replaced: String = name
            .chars()
            .map(|ch| {
                if ch == '/' || ch == '\\' || ch == '\0' || ch.is_control() {
                    '_'
                } else {
                    ch
                }
            })
            .collect();
        replaced.trim().to_string()
    };
    if cleaned.is_empty() || cleaned == "." || cleaned == ".." {
        "共享文件夹".to_string()
    } else {
        cleaned
    }
}

/// 为任务内多个共享目录生成互不冲突的目标子目录名。
/// 名称相同时按顺序追加 ` (2)`、` (3)`，保证重复任务得到相同结果。
pub fn share_directory_names(shares: &[TaskShare]) -> Vec<String> {
    let mut used = HashSet::new();
    let mut out = Vec::with_capacity(shares.len());
    for share in shares {
        let base = safe_share_dir_name(&share.name);
        let mut candidate = base.clone();
        let mut counter = 2u32;
        while !used.insert(candidate.to_lowercase()) {
            candidate = format!("{base} ({counter})");
            counter += 1;
        }
        out.push(candidate);
    }
    out
}

/// 避免同一目录内大小写不敏感卷上的碰撞；`used` 使用小写归一化键。
pub fn unique_dest_path(
    desired: &[String],
    used: &mut BTreeSet<String>,
) -> Result<(Vec<String>, bool)> {
    let mut candidate = desired.to_vec();
    let key = rel_path_from_components(&candidate).to_lowercase();
    if !used.contains(&key) {
        used.insert(key);
        return Ok((candidate, false));
    }

    let file_name = candidate.pop().ok_or(HarborError::InvalidPath)?;
    let (stem, extension) = split_extension(&file_name);
    let mut counter = 2u32;
    loop {
        let name = if counter == 2 && !extension.is_empty() {
            format!("{stem} (2).{extension}")
        } else if extension.is_empty() {
            format!("{stem} ({counter})")
        } else {
            format!("{stem} ({counter}).{extension}")
        };
        candidate.push(name);
        let key = rel_path_from_components(&candidate).to_lowercase();
        if !used.contains(&key) {
            used.insert(key);
            return Ok((candidate, true));
        }
        candidate.pop();
        counter += 1;
        if counter > 10_000 {
            return Err(HarborError::Conflict("同名文件过多".into()));
        }
    }
}

/// 冲突策略「保留两者」的目标名：`报告 (来自 设备 2026-11-08).pdf`。
pub fn conflict_name(original: &str, peer: &str, date: &str) -> String {
    let (stem, extension) = split_extension(original);
    let suffix = format!(" (来自 {peer} {date})");
    if extension.is_empty() {
        format!("{stem}{suffix}")
    } else {
        format!("{stem}{suffix}.{extension}")
    }
}

fn split_extension(name: &str) -> (String, String) {
    match name.rfind('.') {
        Some(index) if index > 0 && index + 1 < name.len() => {
            (name[..index].to_string(), name[index + 1..].to_string())
        }
        _ => (name.to_string(), String::new()),
    }
}

/// 生成 `YYYY-MM-DD`（UTC），用于冲突改名与隔离区展示。
pub fn utc_date(epoch_ms: u64) -> String {
    let days = (epoch_ms / 86_400_000) as i64;
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}")
}

/// Howard Hinnant 的 civil_from_days 算法，避免引入时间库。
fn civil_from_days(days_since_epoch: i64) -> (i64, u32, u32) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// 校验目标路径仍位于根目录内（词法检查，不触碰文件系统）。
pub fn path_is_within(root: &Path, path: &Path) -> bool {
    path.starts_with(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bitmap_roundtrip() {
        let mut bitmap = Bitmap::new(10);
        assert_eq!(bitmap.count(), 0);
        bitmap.set(0);
        bitmap.set(9);
        assert!(bitmap.get(0));
        assert!(!bitmap.get(1));
        assert!(bitmap.get(9));
        assert_eq!(bitmap.missing(), vec![1, 2, 3, 4, 5, 6, 7, 8]);

        let restored = Bitmap::from_bytes(bitmap.as_bytes().to_vec(), 10);
        assert_eq!(restored, bitmap);
    }

    #[test]
    fn rejects_traversal_paths() {
        for bad in ["", "/etc/passwd", "../x", "a/../../b", "a\\..\\b", "a\0b"] {
            assert!(
                matches!(safe_rel_components(bad), Err(HarborError::InvalidPath)),
                "应拒绝 {bad:?}"
            );
        }
        assert_eq!(
            safe_rel_components("目录/子目录/a.txt").unwrap(),
            vec!["目录", "子目录", "a.txt"]
        );
    }

    #[test]
    fn sanitizes_windows_names() {
        assert_eq!(sanitize_windows_component("a:b?.txt"), "a_b_.txt");
        assert_eq!(sanitize_windows_component("CON.txt"), "_CON.txt");
        assert_eq!(sanitize_windows_component("trail. "), "trail");
        assert_eq!(sanitize_windows_component(""), "unnamed");
    }

    #[test]
    fn deduplicates_share_directory_names() {
        let shares = vec![
            TaskShare {
                id: "1".into(),
                name: "Photos".into(),
            },
            TaskShare {
                id: "2".into(),
                name: "photos".into(),
            },
        ];
        assert_eq!(
            share_directory_names(&shares),
            vec!["Photos".to_string(), "photos (2)".to_string()]
        );
    }

    #[test]
    fn deduplicates_case_insensitive_paths() {
        let mut used = BTreeSet::new();
        let first = unique_dest_path(&["A.txt".into()], &mut used).unwrap();
        assert_eq!(first.0, vec!["A.txt"]);
        assert!(!first.1);
        let second = unique_dest_path(&["a.txt".into()], &mut used).unwrap();
        assert_eq!(second.0, vec!["a (2).txt"]);
        assert!(second.1);
    }

    #[test]
    fn conflict_name_keeps_extension() {
        assert_eq!(
            conflict_name("报告.pdf", "MacBook", "2026-11-08"),
            "报告 (来自 MacBook 2026-11-08).pdf"
        );
        assert_eq!(
            conflict_name("README", "MacBook", "2026-11-08"),
            "README (来自 MacBook 2026-11-08)"
        );
    }

    #[test]
    fn formats_utc_date() {
        assert_eq!(utc_date(0), "1970-01-01");
        assert_eq!(utc_date(1_700_000_000_000), "2023-11-14");
    }

    #[test]
    fn part_state_detects_changes() {
        let header = RemoteFileHeader {
            size: 3,
            mtime_ms: Some(1),
            mode: 0o644,
            content_hash: hex::encode([1u8; 32]),
            chunks: vec![
                ChunkInfo {
                    len: 2,
                    hash: hex::encode([2u8; 32]),
                },
                ChunkInfo {
                    len: 1,
                    hash: hex::encode([3u8; 32]),
                },
            ],
            stable: true,
        };
        let state = PartState::new(&header);
        assert!(state.matches(&header));
        assert_eq!(state.missing(), vec![0, 1]);
        assert_eq!(state.received_bytes(&header.chunks), 0);
    }
}
