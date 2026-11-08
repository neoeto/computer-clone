use thiserror::Error;

#[derive(Debug, Error)]
pub enum HarborError {
    #[error("网络错误：{0}")]
    Io(#[from] std::io::Error),

    #[error("协议错误：{0}")]
    Protocol(String),

    #[error("连接码不正确，或已过期")]
    AuthFailed,

    #[error("对方拒绝了这次连接")]
    Denied,

    #[error("等待确认超时")]
    ApprovalTimeout,

    #[error("连接超时")]
    Timeout,

    #[error("路径不合法")]
    InvalidPath,

    #[error("路径不存在：{0}")]
    NotFound(String),

    #[error("{0}")]
    Conflict(String),

    #[error("本地存储失败：{0}")]
    Storage(String),

    #[error("加密错误：{0}")]
    Tls(String),

    #[error("设备名称不能为空")]
    InvalidDeviceName,

    #[error("{0}")]
    Other(String),
}

impl From<rustls::Error> for HarborError {
    fn from(value: rustls::Error) -> Self {
        HarborError::Tls(value.to_string())
    }
}

impl From<serde_json::Error> for HarborError {
    fn from(value: serde_json::Error) -> Self {
        HarborError::Protocol(format!("JSON 编解码失败：{value}"))
    }
}

impl From<rcgen::Error> for HarborError {
    fn from(value: rcgen::Error) -> Self {
        HarborError::Tls(format!("证书生成失败：{value}"))
    }
}

pub type Result<T> = std::result::Result<T, HarborError>;

/// 供 Tauri 层把错误映射为前端可读的错误码。
impl HarborError {
    pub fn code(&self) -> &'static str {
        match self {
            HarborError::AuthFailed => "auth_failed",
            HarborError::Denied => "denied",
            HarborError::ApprovalTimeout => "approval_timeout",
            HarborError::Timeout => "timeout",
            HarborError::InvalidPath => "invalid_path",
            HarborError::NotFound(_) => "not_found",
            HarborError::Conflict(_) => "conflict",
            HarborError::Io(_) => "network",
            HarborError::Protocol(_) => "protocol",
            HarborError::Storage(_) => "storage",
            HarborError::Tls(_) => "crypto",
            HarborError::InvalidDeviceName => "invalid_device_name",
            HarborError::Other(_) => "internal",
        }
    }
}
