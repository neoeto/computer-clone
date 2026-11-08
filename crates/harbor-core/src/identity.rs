use std::fs;
use std::path::Path;

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::error::{HarborError, Result};

/// 本机长期身份：设备 id + 自签 TLS 证书 + Ed25519 设备密钥。
///
/// TLS 证书用于通道加密与指纹展示；Ed25519 密钥用于已信任设备的
/// 签名挑战认证，与 TLS 私钥分离。
pub struct Identity {
    pub device_id: String,
    pub cert_der: Vec<u8>,
    pub key_der: Vec<u8>,
    pub fingerprint: String,
    pub ed25519_seed: [u8; 32],
    pub ed25519_public: [u8; 32],
}

impl Identity {
    pub fn load_or_create(dir: &Path) -> Result<Self> {
        fs::create_dir_all(dir)
            .map_err(|e| HarborError::Storage(format!("创建身份目录失败：{e}")))?;

        let id_path = dir.join("device_id");
        let cert_path = dir.join("tls_cert.der");
        let key_path = dir.join("tls_key.der");
        let device_key_path = dir.join("device_key");

        let device_id = match fs::read_to_string(&id_path) {
            Ok(text) if !text.trim().is_empty() => text.trim().to_string(),
            _ => {
                let id = Uuid::new_v4().to_string();
                write_private(&id_path, id.as_bytes())?;
                id
            }
        };

        let ed25519_seed = match fs::read(&device_key_path) {
            Ok(bytes) if bytes.len() == 32 => {
                let mut seed = [0u8; 32];
                seed.copy_from_slice(&bytes);
                seed
            }
            _ => {
                let mut seed = [0u8; 32];
                for chunk in seed.chunks_mut(8) {
                    chunk.copy_from_slice(&rand::random::<u64>().to_le_bytes());
                }
                write_private(&device_key_path, &seed)?;
                seed
            }
        };
        let ed25519_public = *SigningKey::from_bytes(&ed25519_seed)
            .verifying_key()
            .as_bytes();

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
                    ed25519_seed,
                    ed25519_public,
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
            ed25519_seed,
            ed25519_public,
        })
    }

    pub fn public_key_hex(&self) -> String {
        hex::encode(self.ed25519_public)
    }

    pub fn key_fingerprint(&self) -> String {
        fingerprint(&self.ed25519_public)
    }

    /// 对已信任设备挑战等短消息签名。
    pub fn sign(&self, message: &[u8]) -> String {
        let signing = SigningKey::from_bytes(&self.ed25519_seed);
        hex::encode(signing.sign(message).to_bytes())
    }
}

/// 用设备公钥验证签名；任何格式错误都返回 false。
pub fn verify_signature(public_key_hex: &str, message: &[u8], signature_hex: &str) -> bool {
    let Ok(public) = hex::decode(public_key_hex) else {
        return false;
    };
    let Ok(public) = <[u8; 32]>::try_from(public.as_slice()) else {
        return false;
    };
    let Ok(signature) = hex::decode(signature_hex) else {
        return false;
    };
    let Ok(signature) = <[u8; 64]>::try_from(signature.as_slice()) else {
        return false;
    };
    let Ok(verifying) = VerifyingKey::from_bytes(&public) else {
        return false;
    };
    verifying
        .verify(message, &Signature::from_bytes(&signature))
        .is_ok()
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
        assert_eq!(a.ed25519_public, b.ed25519_public);
        assert!(a.fingerprint.contains(':'));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn signs_and_verifies_challenge() {
        let dir = std::env::temp_dir().join(format!("harbor-id-sign-{}", Uuid::new_v4()));
        let identity = Identity::load_or_create(&dir).unwrap();
        let message = b"harbor-auth-v1 challenge";
        let signature = identity.sign(message);
        assert!(verify_signature(
            &identity.public_key_hex(),
            message,
            &signature
        ));
        assert!(!verify_signature(
            &identity.public_key_hex(),
            b"other message",
            &signature
        ));
        let other = Identity::load_or_create(&dir.join("other")).unwrap();
        assert!(!verify_signature(
            &other.public_key_hex(),
            message,
            &signature
        ));
        fs::remove_dir_all(&dir).ok();
    }
}
