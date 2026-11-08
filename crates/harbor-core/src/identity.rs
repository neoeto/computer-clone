use std::fs;
use std::path::Path;

use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::error::{HarborError, Result};

/// 本机长期身份：设备 id + 自签 TLS 证书。
///
/// M1 用同一张自签证书做 TLS；后续会在此基础上增加 Ed25519 设备密钥，
/// 用于已信任设备免配对认证。
pub struct Identity {
    pub device_id: String,
    pub cert_der: Vec<u8>,
    pub key_der: Vec<u8>,
    pub fingerprint: String,
}

impl Identity {
    pub fn load_or_create(dir: &Path) -> Result<Self> {
        fs::create_dir_all(dir)
            .map_err(|e| HarborError::Storage(format!("创建身份目录失败：{e}")))?;

        let id_path = dir.join("device_id");
        let cert_path = dir.join("tls_cert.der");
        let key_path = dir.join("tls_key.der");

        let device_id = match fs::read_to_string(&id_path) {
            Ok(text) if !text.trim().is_empty() => text.trim().to_string(),
            _ => {
                let id = Uuid::new_v4().to_string();
                write_private(&id_path, id.as_bytes())?;
                id
            }
        };

        if cert_path.exists() && key_path.exists() {
            let cert_der = fs::read(&cert_path)
                .map_err(|e| HarborError::Storage(format!("读取证书失败：{e}")))?;
            let key_der = fs::read(&key_path)
                .map_err(|e| HarborError::Storage(format!("读取私钥失败：{e}")))?;
            if !cert_der.is_empty() && !key_der.is_empty() {
                let fingerprint = fingerprint(&cert_der);
                return Ok(Self {
                    device_id,
                    cert_der,
                    key_der,
                    fingerprint,
                });
            }
        }

        let host = crate::util::default_device_name();
        let san = vec!["harbor.local".to_string(), format!("{host}.local")];
        let certified = rcgen::generate_simple_self_signed(san)?;
        let cert_der = certified.cert.der().to_vec();
        let key_der = certified.signing_key.serialize_der();
        write_private(&cert_path, &cert_der)?;
        write_private(&key_path, &key_der)?;
        let fingerprint = fingerprint(&cert_der);

        Ok(Self {
            device_id,
            cert_der,
            key_der,
            fingerprint,
        })
    }
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    fs::write(path, bytes).map_err(|e| HarborError::Storage(format!("写入身份文件失败：{e}")))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// 证书指纹：SHA-256，大写、冒号分隔。
pub fn fingerprint(cert_der: &[u8]) -> String {
    let digest = Sha256::digest(cert_der);
    digest
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

pub fn short(full: &str) -> String {
    full.split(':').take(8).collect::<Vec<_>>().join(":")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_is_stable() {
        let dir = std::env::temp_dir().join(format!("harbor-id-test-{}", Uuid::new_v4()));
        let a = Identity::load_or_create(&dir).unwrap();
        let b = Identity::load_or_create(&dir).unwrap();
        assert_eq!(a.device_id, b.device_id);
        assert_eq!(a.fingerprint, b.fingerprint);
        assert!(a.fingerprint.contains(':'));
        fs::remove_dir_all(&dir).ok();
    }
}
