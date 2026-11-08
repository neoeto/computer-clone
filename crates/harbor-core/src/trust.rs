//! 已信任设备存储。
//!
//! 首次配对经源端用户批准后，写入设备 id、Ed25519 公钥与展示信息；
//! 之后同一设备用签名挑战即可免连接码连接。存储为原子 JSON，
//! 不包含任何私钥或文件内容。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, RwLock};

use serde::{Deserialize, Serialize};

use crate::error::{HarborError, Result};
use crate::types::DeviceInfo;
use crate::util::now_ms;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustedDevice {
    pub id: String,
    pub name: String,
    pub os: String,
    /// 十六进制 Ed25519 公钥。
    pub pubkey: String,
    /// 公钥的 SHA-256 指纹（冒号分隔，便于展示）。
    pub fingerprint: String,
    pub trusted_at: u64,
    pub last_seen: u64,
    pub revoked: bool,
}

pub struct TrustStore {
    path: PathBuf,
    devices: RwLock<Vec<TrustedDevice>>,
    save_lock: Mutex<()>,
}

impl TrustStore {
    pub fn load(dir: &Path) -> Result<Self> {
        fs::create_dir_all(dir)
            .map_err(|err| HarborError::Storage(format!("创建信任目录失败：{err}")))?;
        let path = dir.join("trusted_devices.json");
        let devices = match fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<Vec<TrustedDevice>>(&text) {
                Ok(devices) => devices,
                Err(err) => {
                    tracing::warn!(?err, "信任设备文件损坏，已重置");
                    Vec::new()
                }
            },
            Err(_) => Vec::new(),
        };
        Ok(Self {
            path,
            devices: RwLock::new(devices),
            save_lock: Mutex::new(()),
        })
    }

    pub fn list(&self) -> Vec<TrustedDevice> {
        let mut out = self
            .devices
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        out.sort_by(|a, b| b.trusted_at.cmp(&a.trusted_at));
        out
    }

    pub fn find(&self, device_id: &str) -> Option<TrustedDevice> {
        self.devices
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .find(|device| device.id == device_id)
            .cloned()
    }

    /// 仅当设备未撤销且公钥完全一致时视为已信任。
    pub fn is_trusted(&self, device_id: &str, pubkey: &str) -> bool {
        self.devices
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .any(|device| {
                device.id == device_id
                    && !device.revoked
                    && device.pubkey.eq_ignore_ascii_case(pubkey)
            })
    }

    pub fn trust(&self, device: &DeviceInfo, pubkey: &str) -> Result<TrustedDevice> {
        let now = now_ms();
        let fingerprint = crate::identity::fingerprint(
            &hex::decode(pubkey).unwrap_or_else(|_| pubkey.as_bytes().to_vec()),
        );
        let mut list = self
            .devices
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(existing) = list.iter_mut().find(|entry| entry.id == device.id) {
            existing.name = device.name.clone();
            existing.os = device.os.clone();
            existing.pubkey = pubkey.to_string();
            existing.fingerprint = fingerprint;
            existing.trusted_at = now;
            existing.last_seen = now;
            existing.revoked = false;
            let out = existing.clone();
            drop(list);
            self.save()?;
            return Ok(out);
        }
        let trusted = TrustedDevice {
            id: device.id.clone(),
            name: device.name.clone(),
            os: device.os.clone(),
            pubkey: pubkey.to_string(),
            fingerprint,
            trusted_at: now,
            last_seen: now,
            revoked: false,
        };
        list.push(trusted.clone());
        drop(list);
        self.save()?;
        Ok(trusted)
    }

    pub fn touch(&self, device_id: &str) {
        let changed = {
            let mut list = self
                .devices
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            match list.iter_mut().find(|device| device.id == device_id) {
                Some(device) => {
                    device.last_seen = now_ms();
                    true
                }
                None => false,
            }
        };
        if changed {
            if let Err(err) = self.save() {
                tracing::warn!(?err, "更新信任设备最近时间失败");
            }
        }
    }

    pub fn revoke(&self, device_id: &str) -> Result<()> {
        {
            let mut list = self
                .devices
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let device = list
                .iter_mut()
                .find(|device| device.id == device_id)
                .ok_or_else(|| HarborError::NotFound(device_id.to_string()))?;
            device.revoked = true;
        }
        self.save()
    }

    fn save(&self) -> Result<()> {
        let _guard = self
            .save_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let list = self
            .devices
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let bytes = serde_json::to_vec_pretty(&list)?;
        let tmp = self.path.with_extension("json.tmp");
        fs::write(&tmp, bytes)
            .map_err(|err| HarborError::Storage(format!("写入信任设备失败：{err}")))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600));
        }
        fs::rename(&tmp, &self.path)
            .map_err(|err| HarborError::Storage(format!("替换信任设备文件失败：{err}")))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device() -> DeviceInfo {
        DeviceInfo {
            id: "peer-1".into(),
            name: "Peer".into(),
            os: "macOS".into(),
            app_version: "0.1.0".into(),
        }
    }

    #[test]
    fn trusts_revokes_and_persists() {
        let dir = std::env::temp_dir().join(format!("harbor-trust-{}", uuid::Uuid::new_v4()));
        let store = TrustStore::load(&dir).unwrap();
        let pubkey = hex::encode([7u8; 32]);
        assert!(!store.is_trusted("peer-1", &pubkey));
        store.trust(&device(), &pubkey).unwrap();
        assert!(store.is_trusted("peer-1", &pubkey));
        assert!(!store.is_trusted("peer-1", &hex::encode([8u8; 32])));

        let reopened = TrustStore::load(&dir).unwrap();
        assert!(reopened.is_trusted("peer-1", &pubkey));
        reopened.revoke("peer-1").unwrap();
        assert!(!reopened.is_trusted("peer-1", &pubkey));
        assert!(reopened.find("peer-1").unwrap().revoked);

        fs::remove_dir_all(&dir).ok();
    }
}
