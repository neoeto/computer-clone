use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::error::{HarborError, Result};
use crate::types::{DeviceInfo, Entry, RemoteShare};

/// 单帧上限 2 MiB。M1 只传 JSON 控制消息，后续文件块走独立数据连接。
pub const MAX_FRAME: usize = 2 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    Hello { protocol: u32, device: DeviceInfo },
    Auth { code: String },
    ListShares,
    Browse { share_id: String, path: String },
    Ping,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    Hello {
        protocol: u32,
        device: DeviceInfo,
        fingerprint: String,
        port: u16,
    },
    /// 连接码已通过，等待源端用户核对并确认。
    AuthPending {
        request_id: String,
        sas: String,
    },
    AuthOk {
        session: String,
    },
    Error {
        code: String,
        message: String,
    },
    Shares {
        shares: Vec<RemoteShare>,
    },
    Browse {
        path: String,
        entries: Vec<Entry>,
    },
    Pong,
}

impl Response {
    pub fn error(code: &str, message: impl Into<String>) -> Self {
        Response::Error {
            code: code.to_string(),
            message: message.into(),
        }
    }
}

/// 写入一帧：`u32 BE 长度` + JSON。
pub async fn write_msg<W, T>(writer: &mut W, msg: &T) -> Result<()>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let payload = serde_json::to_vec(msg)?;
    if payload.len() > MAX_FRAME {
        return Err(HarborError::Protocol("消息过大".into()));
    }
    writer.write_u32(payload.len() as u32).await?;
    writer.write_all(&payload).await?;
    writer.flush().await?;
    Ok(())
}

/// 读取一帧并反序列化。
pub async fn read_msg<R, S>(reader: &mut S) -> Result<R>
where
    R: DeserializeOwned,
    S: AsyncRead + Unpin,
{
    let len = reader.read_u32().await? as usize;
    if len == 0 || len > MAX_FRAME {
        return Err(HarborError::Protocol(format!("非法帧长度：{len}")));
    }
    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf).await?;
    Ok(serde_json::from_slice(&buf)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn roundtrip() {
        let (mut a, mut b) = tokio::io::duplex(4096);
        let req = Request::Auth {
            code: "123456".into(),
        };
        write_msg(&mut a, &req).await.unwrap();
        let got: Request = read_msg(&mut b).await.unwrap();
        match got {
            Request::Auth { code } => assert_eq!(code, "123456"),
            _ => panic!("unexpected"),
        }
    }

    #[tokio::test]
    async fn rejects_oversize_frame() {
        let (mut a, mut b) = tokio::io::duplex(64);
        a.write_u32((MAX_FRAME + 1) as u32).await.unwrap();
        let err = read_msg::<Request, _>(&mut b).await.unwrap_err();
        assert!(matches!(err, HarborError::Protocol(_)));
    }
}
