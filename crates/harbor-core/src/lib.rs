//! 渡口 Harbor 核心库：局域网发现、TLS 连接、配对审批、共享目录浏览。
//!
//! 该 crate 不依赖 Tauri，可单独测试，也便于后续抽成 CLI 或移动端核心。

pub mod audit;
pub mod chunker;
pub mod client;
pub mod config;
pub mod data;
pub mod discovery;
pub mod engine;
pub mod error;
pub mod identity;
pub mod pairing;
pub mod protocol;
pub mod receive;
pub mod server;
pub mod shares;
pub mod sync;
pub mod tasks;
pub mod trust;
pub mod types;
pub mod util;

pub use engine::Engine;
pub use error::{HarborError, Result};
pub use types::*;

/// mDNS 服务类型。
pub const SERVICE_TYPE: &str = "_harbor._tcp.local.";
/// 协议版本，握手时校验。
pub const PROTOCOL_VERSION: u32 = 2;
/// 默认监听端口。
pub const DEFAULT_PORT: u16 = 47821;
/// 应用版本（展示用）。
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
/// 连接码有效期（秒）。
pub const CODE_TTL_SECS: u64 = 600;
/// 单次连接错码上限。
pub const MAX_CODE_ATTEMPTS: u32 = 5;
/// 等待源端确认的超时（秒）。
pub const APPROVAL_TIMEOUT_SECS: u64 = 60;
