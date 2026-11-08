use serde::{Deserialize, Serialize};

use crate::sync::{FileProgress, TaskReport, TaskSummary};

/// 设备身份信息，随握手交换。不包含任何硬件序列号。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
    pub os: String,
    pub app_version: String,
}

/// 源端本地看到的共享目录（含本机绝对路径，仅本机 UI 使用）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShareSummary {
    pub id: String,
    pub name: String,
    pub path: String,
    pub enabled: bool,
    pub file_count: u64,
    pub total_bytes: u64,
}

/// 发送给目的端的共享目录摘要，刻意不包含路径。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteShare {
    pub id: String,
    pub name: String,
    pub file_count: u64,
    pub total_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    /// 文件名（不含路径）。
    pub name: String,
    /// 相对共享根目录的路径，使用 `/` 分隔。
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectedClient {
    pub device: DeviceInfo,
    pub address: String,
    pub connected_at: u64,
}

/// 源端首页一次性需要的全部信息。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceInfo {
    pub running: bool,
    pub device: DeviceInfo,
    pub port: u16,
    pub addresses: Vec<String>,
    pub fingerprint: String,
    pub code: String,
    pub code_expires_in: u64,
    pub shares: Vec<ShareSummary>,
    pub clients: Vec<ConnectedClient>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteSourceInfo {
    pub device: DeviceInfo,
    pub address: String,
    pub port: u16,
    pub fingerprint: String,
    pub connected_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveredSource {
    pub device_id: String,
    pub name: String,
    pub os: String,
    pub host: String,
    pub address: String,
    pub port: u16,
    pub fingerprint: String,
    pub last_seen: u64,
    #[serde(skip)]
    pub fullname: String,
}

/// 核心层推给 UI 的事件。Tauri 层原样转发到 `harbor://event`。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CoreEvent {
    ServiceStarted {
        info: SourceInfo,
    },
    ServiceStopped,
    /// 源端收到新设备请求，需要用户核对 SAS 后批准。
    ConnectionRequest {
        request_id: String,
        device: DeviceInfo,
        address: String,
        sas: String,
    },
    ClientConnected {
        device: DeviceInfo,
        address: String,
    },
    ClientDisconnected {
        device: DeviceInfo,
        address: String,
    },
    /// 目的端已拿到 SAS，正在等待源端确认。
    ConnectPending {
        connect_id: String,
        source: RemoteSourceInfo,
        sas: String,
    },
    ConnectSucceeded {
        connect_id: String,
        session_id: String,
        source: RemoteSourceInfo,
    },
    ConnectFailed {
        connect_id: String,
        message: String,
    },
    Discovered {
        sources: Vec<DiscoveredSource>,
    },
    /// 任务状态变化（创建、暂停、恢复、中断、完成）。
    TaskUpdated {
        task: TaskSummary,
    },
    /// 传输中的聚合进度，最多约 4 次/秒。
    TaskProgress {
        task: TaskSummary,
        /// 当前正在传输/排队/失败的文件，最多 8 条。
        files: Vec<FileProgress>,
    },
    TaskFinished {
        task: TaskSummary,
        report: TaskReport,
    },
    TaskRemoved {
        task_id: String,
    },
    /// 源端正在向某设备发送文件（每个会话最多 1 秒推送一次）。
    TransferServed {
        device: DeviceInfo,
        path: String,
        size: u64,
    },
    Error {
        message: String,
    },
}

/// 保存完整指纹，UI 可截断显示。
pub fn short_fingerprint(full: &str) -> String {
    full.split(':').take(8).collect::<Vec<_>>().join(":")
}
