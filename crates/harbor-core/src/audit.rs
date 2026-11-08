//! 持久化审计日志。
//!
//! 只记录连接、审批、信任与任务结果等元数据，不记录文件内容或连接码。
//! 采用追加式 JSONL，启动时加载最近 2000 条；后续可平滑迁移到 SQLite。

use std::collections::VecDeque;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::error::{HarborError, Result};
use crate::types::DeviceInfo;
use crate::util::now_ms;

const MAX_EVENTS: usize = 2000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEvent {
    pub id: u64,
    pub at: u64,
    pub kind: String,
    /// `ok` / `info` / `warn` / `error`
    pub level: String,
    pub device_id: Option<String>,
    pub device_name: Option<String>,
    pub address: Option<String>,
    pub detail: String,
}

pub struct AuditLog {
    path: PathBuf,
    events: Mutex<VecDeque<AuditEvent>>,
    next_id: AtomicU64,
}

impl AuditLog {
    pub fn load(dir: &Path) -> Result<Self> {
        fs::create_dir_all(dir)
            .map_err(|err| HarborError::Storage(format!("创建审计目录失败：{err}")))?;
        let path = dir.join("audit.jsonl");
        let mut events = VecDeque::new();
        let mut max_id = 0u64;
        if let Ok(text) = fs::read_to_string(&path) {
            for line in text
                .lines()
                .rev()
                .take(MAX_EVENTS)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
            {
                if line.trim().is_empty() {
                    continue;
                }
                if let Ok(event) = serde_json::from_str::<AuditEvent>(line) {
                    max_id = max_id.max(event.id);
                    events.push_back(event);
                }
            }
        }
        Ok(Self {
            path,
            events: Mutex::new(events),
            next_id: AtomicU64::new(max_id + 1),
        })
    }

    pub fn record(
        &self,
        kind: &str,
        level: &str,
        device: Option<&DeviceInfo>,
        address: Option<&str>,
        detail: impl Into<String>,
    ) {
        let event = AuditEvent {
            id: self.next_id.fetch_add(1, Ordering::SeqCst),
            at: now_ms(),
            kind: kind.to_string(),
            level: level.to_string(),
            device_id: device.map(|device| device.id.clone()),
            device_name: device.map(|device| device.name.clone()),
            address: address.map(str::to_string),
            detail: detail.into(),
        };
        if let Ok(mut events) = self.events.lock() {
            events.push_back(event.clone());
            while events.len() > MAX_EVENTS {
                events.pop_front();
            }
        }
        let append = serde_json::to_vec(&event).map(|mut bytes| {
            bytes.push(b'\n');
            bytes
        });
        if let Ok(bytes) = append {
            if let Err(err) = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)
                .and_then(|mut file| file.write_all(&bytes))
            {
                tracing::warn!(?err, "写入审计日志失败");
            }
        }
    }

    /// 最近事件，倒序。
    pub fn list(&self, limit: usize) -> Vec<AuditEvent> {
        let events = self
            .events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        events
            .iter()
            .rev()
            .take(limit.min(MAX_EVENTS))
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_and_reloads() {
        let dir = std::env::temp_dir().join(format!("harbor-audit-{}", uuid::Uuid::new_v4()));
        let log = AuditLog::load(&dir).unwrap();
        let device = DeviceInfo {
            id: "peer".into(),
            name: "Peer".into(),
            os: "macOS".into(),
            app_version: "0.1.0".into(),
        };
        log.record("connected", "ok", Some(&device), Some("127.0.0.1"), "ok");
        log.record("auth_failed", "warn", None, None, "bad code");
        let events = log.list(10);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].kind, "auth_failed");

        let reopened = AuditLog::load(&dir).unwrap();
        let events = reopened.list(10);
        assert_eq!(events.len(), 2);
        assert_eq!(events[1].device_name.as_deref(), Some("Peer"));
        fs::remove_dir_all(&dir).ok();
    }
}
