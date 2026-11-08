use std::time::Instant;

use spake2::{Ed25519Group, Identity as Spake2Identity, Password, Spake2};

use crate::error::{HarborError, Result};
use crate::{CODE_TTL_SECS, MAX_CODE_ATTEMPTS};

/// 一次性 6 位连接码。M1 通过 TLS 传输后做常量时间比较；
/// 后续 M3 会替换为 SPAKE2，码本身不再出现在线路上。
pub struct PairingCode {
    code: String,
    issued_at: Instant,
    attempts: u32,
    used: bool,
}

impl PairingCode {
    pub fn new() -> Self {
        Self {
            code: random_code(),
            issued_at: Instant::now(),
            attempts: 0,
            used: false,
        }
    }

    pub fn code(&self) -> &str {
        &self.code
    }

    /// SPAKE2 使用的当前有效口令；过期或已锁定时返回 `None`。
    pub fn usable_secret(&self) -> Option<&str> {
        if self.expired() || self.attempts >= MAX_CODE_ATTEMPTS {
            None
        } else {
            Some(&self.code)
        }
    }

    pub fn record_failure(&mut self) {
        self.attempts = self.attempts.saturating_add(1);
    }

    pub fn seconds_left(&self) -> u64 {
        if self.used {
            return 0;
        }
        CODE_TTL_SECS.saturating_sub(self.issued_at.elapsed().as_secs())
    }

    pub fn expired(&self) -> bool {
        self.used || self.seconds_left() == 0
    }

    /// 校验并累加失败次数。成功不代表消费，调用方需再调用 `consume`。
    pub fn verify(&mut self, input: &str) -> bool {
        if self.expired() || self.attempts >= MAX_CODE_ATTEMPTS {
            return false;
        }
        let normalized: String = input.chars().filter(|c| c.is_ascii_digit()).collect();
        self.attempts += 1;
        constant_time_eq(normalized.as_bytes(), self.code.as_bytes())
    }

    pub fn consume(&mut self) {
        self.used = true;
    }

    pub fn attempts(&self) -> u32 {
        self.attempts
    }
}

impl Default for PairingCode {
    fn default() -> Self {
        Self::new()
    }
}

pub fn random_code() -> String {
    format!("{:06}", rand::random::<u32>() % 1_000_000)
}

/* ------------------------------------------------------------------ */
/* SPAKE2 配对                                                         */
/* ------------------------------------------------------------------ */

const SPAKE2_LABEL: &[u8] = b"harbor-spake2-v1";

/// 一侧的 SPAKE2 协商状态。消息在 `begin_*` 时生成，必须原样发送。
pub struct PairingStart {
    state: Spake2<Ed25519Group>,
    base: Vec<u8>,
    own_message: Vec<u8>,
    client: bool,
}

/// 双方派生出的配对材料。
pub struct PairingMaterial {
    key: [u8; 32],
    transcript: Vec<u8>,
}

impl PairingMaterial {
    /// 目的端确认值。
    pub fn confirm_client(&self) -> String {
        self.confirm(b"client")
    }

    /// 源端确认值。
    pub fn confirm_server(&self) -> String {
        self.confirm(b"server")
    }

    fn confirm(&self, role: &[u8]) -> String {
        let mut input = Vec::with_capacity(role.len() + self.transcript.len() + 16);
        input.extend_from_slice(b"harbor-confirm-v1");
        input.push(0);
        input.extend_from_slice(role);
        input.push(0);
        input.extend_from_slice(&self.transcript);
        hex::encode(blake3::keyed_hash(&self.key, &input).as_bytes())
    }

    /// 双端展示的 6 位 SAS，仅由配对密钥与 transcript 派生。
    pub fn sas(&self) -> String {
        let mut input = Vec::with_capacity(self.transcript.len() + 16);
        input.extend_from_slice(b"harbor-sas-v1");
        input.push(0);
        input.extend_from_slice(&self.transcript);
        let hash = blake3::keyed_hash(&self.key, &input);
        let value = u32::from_be_bytes([
            hash.as_bytes()[0],
            hash.as_bytes()[1],
            hash.as_bytes()[2],
            hash.as_bytes()[3],
        ]);
        format!("{:06}", value % 1_000_000)
    }
}

/// 目的端（SPAKE2 A 方）开始配对。
pub fn begin_client(
    code: &str,
    client_id: &str,
    server_id: &str,
) -> Result<(PairingStart, String)> {
    begin(code, client_id, server_id, true)
}

/// 源端（SPAKE2 B 方）开始配对。
pub fn begin_server(
    code: &str,
    client_id: &str,
    server_id: &str,
) -> Result<(PairingStart, String)> {
    begin(code, client_id, server_id, false)
}

fn begin(
    code: &str,
    client_id: &str,
    server_id: &str,
    client: bool,
) -> Result<(PairingStart, String)> {
    if code.is_empty() || client_id.is_empty() || server_id.is_empty() {
        return Err(HarborError::AuthFailed);
    }
    let password = Password::new(code.as_bytes());
    let id_a = Spake2Identity::new(client_id.as_bytes());
    let id_b = Spake2Identity::new(server_id.as_bytes());
    let (state, message) = if client {
        Spake2::<Ed25519Group>::start_a(&password, &id_a, &id_b)
    } else {
        Spake2::<Ed25519Group>::start_b(&password, &id_a, &id_b)
    };
    let mut base = Vec::with_capacity(SPAKE2_LABEL.len() + client_id.len() + server_id.len() + 2);
    base.extend_from_slice(SPAKE2_LABEL);
    base.push(0);
    base.extend_from_slice(client_id.as_bytes());
    base.push(0);
    base.extend_from_slice(server_id.as_bytes());
    Ok((
        PairingStart {
            state,
            base,
            own_message: message.clone(),
            client,
        },
        hex::encode(message),
    ))
}

impl PairingStart {
    /// 收到对端消息后完成握手。
    pub fn finish(self, peer_message_hex: &str) -> Result<PairingMaterial> {
        let peer = hex::decode(peer_message_hex.trim()).map_err(|_| HarborError::AuthFailed)?;
        if peer.is_empty() || peer.len() > 256 {
            return Err(HarborError::AuthFailed);
        }
        let shared = self
            .state
            .finish(&peer)
            .map_err(|_| HarborError::AuthFailed)?;
        // transcript 采用固定 A||B 顺序，两端计算出完全一致的确认值与 SAS。
        let mut transcript = self.base;
        if self.client {
            transcript.extend_from_slice(&self.own_message);
            transcript.extend_from_slice(&peer);
        } else {
            transcript.extend_from_slice(&peer);
            transcript.extend_from_slice(&self.own_message);
        }
        let key = blake3::derive_key("harbor spake2 v1", &shared);
        Ok(PairingMaterial { key, transcript })
    }
}

/// TLS exporter 的上下文，绑定双方设备 id。
pub fn auth_context(client_id: &str, server_id: &str) -> Vec<u8> {
    let mut context = Vec::with_capacity(client_id.len() + server_id.len() + 1);
    context.extend_from_slice(client_id.as_bytes());
    context.push(0);
    context.extend_from_slice(server_id.as_bytes());
    context
}

/// 已信任设备签名挑战的域分隔消息。
///
/// `exporter` 来自当前 TLS 连接的 `export_keying_material`，使签名不能跨连接重放，
/// 也能阻止终止 TLS 的中间人转发签名。
pub fn auth_message(challenge: &str, client_id: &str, server_id: &str, exporter: &[u8]) -> Vec<u8> {
    let mut message = Vec::with_capacity(
        challenge.len() + client_id.len() + server_id.len() + exporter.len() + 64,
    );
    message.extend_from_slice(b"harbor-auth-v1");
    message.push(0);
    message.extend_from_slice(challenge.as_bytes());
    message.push(0);
    message.extend_from_slice(client_id.as_bytes());
    message.push(0);
    message.extend_from_slice(server_id.as_bytes());
    message.push(0);
    message.extend_from_slice(exporter);
    message
}

/// 32 字节随机挑战。
pub fn random_challenge() -> String {
    format!(
        "{:016x}{:016x}{:016x}{:016x}",
        rand::random::<u64>(),
        rand::random::<u64>(),
        rand::random::<u64>(),
        rand::random::<u64>()
    )
}

/// 源端在已信任设备挑战上签名的域分隔消息。
pub fn server_auth_message(
    challenge: &str,
    client_id: &str,
    server_id: &str,
    exporter: &[u8],
) -> Vec<u8> {
    let mut message = Vec::with_capacity(
        challenge.len() + client_id.len() + server_id.len() + exporter.len() + 64,
    );
    message.extend_from_slice(b"harbor-server-auth-v1");
    message.push(0);
    message.extend_from_slice(challenge.as_bytes());
    message.push(0);
    message.extend_from_slice(client_id.as_bytes());
    message.push(0);
    message.extend_from_slice(server_id.as_bytes());
    message.push(0);
    message.extend_from_slice(exporter);
    message
}

/// 常量时间字符串比较（用于确认值）。
pub fn secure_eq(a: &str, b: &str) -> bool {
    constant_time_eq(a.as_bytes(), b.as_bytes())
}

/// 双端展示的 SAS 校验码（6 位，空格分隔由 UI 处理）。
pub fn sas_code() -> String {
    format!("{:06}", rand::random::<u32>() % 1_000_000)
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verifies_once_and_locks_after_failures() {
        let mut code = PairingCode::new();
        let good = code.code().to_string();
        assert!(code.verify(&good));
        code.consume();
        assert!(!code.verify(&good));

        let mut code = PairingCode::new();
        let good = code.code().to_string();
        for _ in 0..MAX_CODE_ATTEMPTS {
            assert!(!code.verify("000000"));
        }
        assert!(!code.verify(&good));
    }

    #[test]
    fn spake2_both_sides_agree() {
        let (client, message_a) = begin_client("482913", "client-id", "server-id").unwrap();
        let (server, message_b) = begin_server("482913", "client-id", "server-id").unwrap();
        let client = client.finish(&message_b).unwrap();
        let server = server.finish(&message_a).unwrap();
        assert_eq!(client.confirm_client(), server.confirm_client());
        assert_eq!(client.confirm_server(), server.confirm_server());
        assert_eq!(client.sas(), server.sas());
        assert_eq!(client.sas().len(), 6);
    }

    #[test]
    fn spake2_wrong_code_does_not_confirm() {
        let (client, message_a) = begin_client("111111", "client-id", "server-id").unwrap();
        let (server, message_b) = begin_server("222222", "client-id", "server-id").unwrap();
        let client = client.finish(&message_b).unwrap();
        let server = server.finish(&message_a).unwrap();
        assert_ne!(client.confirm_client(), server.confirm_client());
        assert_ne!(client.sas(), server.sas());
    }

    #[test]
    fn auth_message_is_domain_separated() {
        let one = auth_message("challenge", "client", "server", b"exporter");
        let two = auth_message("challenge", "client", "other", b"exporter");
        let three = auth_message("challenge", "client", "server", b"other");
        assert_ne!(one, two);
        assert_ne!(one, three);
        assert!(one.starts_with(b"harbor-auth-v1"));
    }

    #[test]
    fn normalizes_spaces() {
        let mut code = PairingCode::new();
        let good = code.code().to_string();
        let spaced = format!("{} {}", &good[..3], &good[3..]);
        assert!(code.verify(&spaced));
    }
}
