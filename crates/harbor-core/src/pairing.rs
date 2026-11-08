use std::time::Instant;

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
    fn normalizes_spaces() {
        let mut code = PairingCode::new();
        let good = code.code().to_string();
        let spaced = format!("{} {}", &good[..3], &good[3..]);
        assert!(code.verify(&spaced));
    }
}
