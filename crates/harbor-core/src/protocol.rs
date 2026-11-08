use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::error::{HarborError, Result};
use crate::sync::ManifestPage;
use crate::types::{DeviceInfo, Entry, RemoteShare};

/// 单帧上限 2 MiB。文件块走独立数据连接，不占用该上限。
pub const MAX_FRAME: usize = 2 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    Hello {
        protocol: u32,
        device: DeviceInfo,
        /// 十六进制 Ed25519 公钥，用于已信任设备免配对。
        pubkey: String,
    },
    /// 数据连接专用：一次性票据。
    DataHello {
        token: String,
    },
    /// SPAKE2 第一步（目的端 A 方）。
    PairBegin {
        message: String,
    },
    /// SPAKE2 第二步确认（目的端）。
    PairFinish {
        confirm: String,
    },
    /// 已信任设备对挑战签名。
    AuthResponse {
        signature: String,
    },
    ListShares,
    Browse {
        share_id: String,
        path: String,
    },
    /// 分页拉取某共享目录的文件清单；`cursor` 为空表示第一页。
    Manifest {
        share_id: String,
        cursor: Option<String>,
    },
    /// 申请一条数据连接的一次性票据。
    DataTicket,
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
        /// 源端判断该设备是否已信任；为 true 时紧接着发送 `auth_challenge`。
        trusted: bool,
        /// 源端 Ed25519 公钥。
        pubkey: String,
    },
    /// SPAKE2 第二步（源端 B 方）。
    PairChallenge {
        message: String,
    },
    /// SPAKE2 成功：源端确认值 + 待审批请求 + 双端 SAS。
    PairFinishAck {
        confirm: String,
        request_id: String,
        sas: String,
    },
    /// 已信任设备的随机挑战与源端签名。
    AuthChallenge {
        challenge: String,
        signature: String,
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
    ManifestPage {
        page: ManifestPage,
    },
    /// 数据连接票据；`expires_in` 为秒。
    DataTicket {
        token: String,
        expires_in: u64,
    },
    /// 数据连接认证通过。
    DataReady,
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
        let req = Request::PairFinish {
            confirm: "abc".into(),
        };
        write_msg(&mut a, &req).await.unwrap();
        let got: Request = read_msg(&mut b).await.unwrap();
        match got {
            Request::PairFinish { confirm } => assert_eq!(confirm, "abc"),
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
