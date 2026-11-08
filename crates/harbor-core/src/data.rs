//! 数据连接：独立 TLS 通道 + 二进制分块帧。
//!
//! 控制连接只负责清单、票据与任务控制；文件内容走单独的数据连接。
//! 数据连接使用一次性票据认证（30 秒有效、绑定对端 IP），帧格式：
//!
//! ```text
//! +--------+--------+--------------------------------------+
//! | len:u32| kind:u8| payload (JSON / [index:u32][data])   |
//! +--------+--------+--------------------------------------+
//! ```
//!
//! `len` 包含 kind 与 payload。kind=0 为 JSON 控制消息，kind=1 为文件块。

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::client::TlsStream;
use tokio_util::sync::CancellationToken;

use crate::chunker::{self, CHUNK_MAX};
use crate::client;
use crate::engine::{Engine, SourceSession};
use crate::error::{HarborError, Result};
use crate::protocol::{read_msg, write_msg, Request, Response};
use crate::shares;
use crate::sync::{ChunkInfo, RemoteFileHeader};

pub const MAX_DATA_FRAME: usize = CHUNK_MAX + 256;
/// 单次最多请求的块数（约 32 MiB 在途）。
pub const CHUNK_BATCH: usize = 32;
/// 单次最多拉取的块元数据条数。
pub const CHUNK_META_PAGE: u32 = 4096;
const DATA_HANDSHAKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
const DATA_OP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);
const MAX_OPEN_FILE_CHUNKS: u32 = 4_000_000;

const KIND_JSON: u8 = 0;
const KIND_CHUNK: u8 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DataRequest {
    FileOpen { share_id: String, path: String },
    ChunkMeta { start: u32, count: u32 },
    ChunkRequest { indices: Vec<u32> },
    FileClose,
    Ping,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DataResponse {
    FileHeader {
        size: u64,
        mtime_ms: Option<u64>,
        mode: u32,
        content_hash: String,
        chunk_count: u32,
        stable: bool,
    },
    ChunkMeta {
        start: u32,
        chunks: Vec<ChunkInfo>,
    },
    ChunksDone {
        count: u32,
    },
    Closed,
    Pong,
    Error {
        code: String,
        message: String,
    },
}

impl DataResponse {
    fn error(code: &str, message: impl Into<String>) -> Self {
        Self::Error {
            code: code.to_string(),
            message: message.into(),
        }
    }
}

enum Frame {
    Json(Vec<u8>),
    Chunk { index: u32, data: Vec<u8> },
}

async fn write_frame_json<W, T>(writer: &mut W, msg: &T) -> Result<()>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let payload = serde_json::to_vec(msg)?;
    let len = payload
        .len()
        .checked_add(1)
        .ok_or_else(|| HarborError::Protocol("数据帧过大".into()))?;
    if len > MAX_DATA_FRAME {
        return Err(HarborError::Protocol("数据帧过大".into()));
    }
    writer.write_u32(len as u32).await?;
    writer.write_u8(KIND_JSON).await?;
    writer.write_all(&payload).await?;
    writer.flush().await?;
    Ok(())
}

async fn write_frame_chunk<W>(writer: &mut W, index: u32, data: &[u8]) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    if data.len() > CHUNK_MAX {
        return Err(HarborError::Protocol("分块超过上限".into()));
    }
    let len = 1 + 4 + data.len();
    if len > MAX_DATA_FRAME {
        return Err(HarborError::Protocol("分块帧过大".into()));
    }
    writer.write_u32(len as u32).await?;
    writer.write_u8(KIND_CHUNK).await?;
    writer.write_u32(index).await?;
    writer.write_all(data).await?;
    writer.flush().await?;
    Ok(())
}

async fn read_frame<R>(reader: &mut R) -> Result<Frame>
where
    R: AsyncRead + Unpin,
{
    let len = reader.read_u32().await? as usize;
    if len == 0 || len > MAX_DATA_FRAME {
        return Err(HarborError::Protocol(format!("非法数据帧长度：{len}")));
    }
    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf).await?;
    match buf[0] {
        KIND_JSON => Ok(Frame::Json(buf[1..].to_vec())),
        KIND_CHUNK => {
            if buf.len() < 5 {
                return Err(HarborError::Protocol("分块帧缺少块索引".into()));
            }
            let index = u32::from_be_bytes([buf[1], buf[2], buf[3], buf[4]]);
            Ok(Frame::Chunk {
                index,
                data: buf[5..].to_vec(),
            })
        }
        other => Err(HarborError::Protocol(format!("未知数据帧类型：{other}"))),
    }
}

async fn read_json<R, T>(reader: &mut R) -> Result<T>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    match read_frame(reader).await? {
        Frame::Json(bytes) => Ok(serde_json::from_slice(&bytes)?),
        Frame::Chunk { .. } => Err(HarborError::Protocol("此处不应出现文件块".into())),
    }
}

/* ------------------------------------------------------------------ */
/* 源端：数据连接服务                                                  */
/* ------------------------------------------------------------------ */

struct OpenSourceFile {
    file: tokio::fs::File,
    header: RemoteFileHeader,
    offsets: Vec<u64>,
}

/// 源端数据连接主循环。调用前已完成票据认证。
pub async fn serve<S>(engine: Arc<Engine>, mut stream: S, session: Arc<SourceSession>) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    if !session.try_begin_data() {
        let _ = write_frame_json(
            &mut stream,
            &DataResponse::error("too_many_connections", "数据连接数超过上限"),
        )
        .await;
        return Ok(());
    }
    let _guard = DataConnectionGuard(session.clone());
    let mut open: Option<OpenSourceFile> = None;

    loop {
        let request: DataRequest = tokio::select! {
            _ = session.cancel.cancelled() => return Ok(()),
            result = tokio::time::timeout(DATA_OP_TIMEOUT, read_json(&mut stream)) => {
                match result {
                    Ok(Ok(request)) => request,
                    Ok(Err(err)) => {
                        tracing::debug!(?err, "数据连接读取失败");
                        return Ok(());
                    }
                    Err(_) => return Ok(()),
                }
            }
        };

        match request {
            DataRequest::FileOpen { share_id, path } => {
                open = None;
                match open_source_file(&engine, &share_id, &path).await {
                    Ok(state) => {
                        if session.should_emit_transfer() {
                            let _ = engine.events.send(crate::types::CoreEvent::TransferServed {
                                device: session.device.clone(),
                                path: path.clone(),
                                size: state.header.size,
                            });
                        }
                        let response = DataResponse::FileHeader {
                            size: state.header.size,
                            mtime_ms: state.header.mtime_ms,
                            mode: state.header.mode,
                            content_hash: state.header.content_hash.clone(),
                            chunk_count: state.header.chunks.len() as u32,
                            stable: state.header.stable,
                        };
                        open = Some(state);
                        if send(&mut stream, &session, &response).await.is_err() {
                            return Ok(());
                        }
                    }
                    Err(err) => {
                        let response = DataResponse::error(err.code(), err.to_string());
                        if send(&mut stream, &session, &response).await.is_err() {
                            return Ok(());
                        }
                    }
                }
            }
            DataRequest::ChunkMeta { start, count } => {
                let response = match open.as_ref() {
                    None => DataResponse::error("no_file", "尚未打开文件"),
                    Some(state) => {
                        let total = state.header.chunks.len() as u32;
                        let end = start
                            .saturating_add(count.min(crate::sync::CHUNK_META_PAGE_SIZE))
                            .min(total);
                        let chunks = if start >= total {
                            Vec::new()
                        } else {
                            state.header.chunks[start as usize..end as usize].to_vec()
                        };
                        DataResponse::ChunkMeta { start, chunks }
                    }
                };
                if send(&mut stream, &session, &response).await.is_err() {
                    return Ok(());
                }
            }
            DataRequest::ChunkRequest { indices } => {
                let Some(state) = open.as_mut() else {
                    let response = DataResponse::error("no_file", "尚未打开文件");
                    if send(&mut stream, &session, &response).await.is_err() {
                        return Ok(());
                    }
                    continue;
                };
                if indices.len() > 128 {
                    let response = DataResponse::error("too_many_chunks", "单次请求块数过多");
                    if send(&mut stream, &session, &response).await.is_err() {
                        return Ok(());
                    }
                    continue;
                }

                let mut sent = 0u32;
                let mut failure = None;
                for index in indices {
                    let Some(chunk) = state.header.chunks.get(index as usize) else {
                        failure = Some(DataResponse::error("bad_chunk", "块索引超出范围"));
                        break;
                    };
                    let offset = state.offsets[index as usize];
                    let data = match read_chunk(&mut state.file, offset, chunk.len as usize).await {
                        Ok(data) => data,
                        Err(err) => {
                            failure = Some(DataResponse::error("read_failed", err.to_string()));
                            break;
                        }
                    };
                    write_chunk_guarded(&mut stream, &session, index, &data).await?;
                    sent += 1;
                }

                let response = failure.unwrap_or(DataResponse::ChunksDone { count: sent });
                if send(&mut stream, &session, &response).await.is_err() {
                    return Ok(());
                }
            }
            DataRequest::FileClose => {
                open = None;
                if send(&mut stream, &session, &DataResponse::Closed)
                    .await
                    .is_err()
                {
                    return Ok(());
                }
            }
            DataRequest::Ping => {
                if send(&mut stream, &session, &DataResponse::Pong)
                    .await
                    .is_err()
                {
                    return Ok(());
                }
            }
        }
    }
}

struct DataConnectionGuard(Arc<SourceSession>);

impl Drop for DataConnectionGuard {
    fn drop(&mut self) {
        self.0.end_data();
    }
}

async fn send<W: AsyncWrite + Unpin>(
    stream: &mut W,
    session: &SourceSession,
    response: &DataResponse,
) -> Result<()> {
    tokio::select! {
        _ = session.cancel.cancelled() => Err(HarborError::Cancelled),
        result = write_frame_json(stream, response) => result,
    }
}

async fn write_chunk_guarded<W: AsyncWrite + Unpin>(
    stream: &mut W,
    session: &SourceSession,
    index: u32,
    data: &[u8],
) -> Result<()> {
    tokio::select! {
        _ = session.cancel.cancelled() => Err(HarborError::Cancelled),
        result = write_frame_chunk(stream, index, data) => result,
    }
}

async fn read_chunk(file: &mut tokio::fs::File, offset: u64, len: usize) -> Result<Vec<u8>> {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};
    file.seek(std::io::SeekFrom::Start(offset)).await?;
    let mut data = vec![0u8; len];
    file.read_exact(&mut data).await?;
    Ok(data)
}

async fn open_source_file(engine: &Engine, share_id: &str, rel: &str) -> Result<OpenSourceFile> {
    let root: PathBuf = engine.source_share_root(share_id).await?;
    let path = shares::resolve_file_in_share(&root, rel)?;

    let path_for_chunks = path.clone();
    let chunks = tokio::task::spawn_blocking(move || chunker::chunk_file(&path_for_chunks))
        .await
        .map_err(|err| HarborError::Other(format!("分块任务失败：{err}")))??;
    if chunks.chunks.len() > MAX_OPEN_FILE_CHUNKS as usize {
        return Err(HarborError::Protocol("文件块数超过上限".into()));
    }

    let meta = std::fs::metadata(&path)?;
    let mtime_ms = meta
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis() as u64);
    let mode = file_mode(&meta);

    let file = tokio::fs::File::open(&path).await?;
    let header = RemoteFileHeader {
        size: chunks.size,
        mtime_ms,
        mode,
        content_hash: hex::encode(chunks.content_hash),
        chunks: chunks.chunks.iter().map(ChunkInfo::from_chunk).collect(),
        stable: true,
    };
    let offsets = crate::sync::chunk_offsets(&header.chunks);
    Ok(OpenSourceFile {
        file,
        header,
        offsets,
    })
}

#[cfg(unix)]
fn file_mode(meta: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o777
}

#[cfg(not(unix))]
fn file_mode(_meta: &std::fs::Metadata) -> u32 {
    0
}

/* ------------------------------------------------------------------ */
/* 目的端：数据连接客户端                                              */
/* ------------------------------------------------------------------ */

pub struct RemoteData {
    stream: TlsStream<TcpStream>,
    cancel: CancellationToken,
}

impl RemoteData {
    /// 申请票据后建立数据连接。`expected_fingerprint` 与发现记录比对。
    pub async fn connect(
        host: &str,
        port: u16,
        expected_fingerprint: Option<&str>,
        token: &str,
        cancel: CancellationToken,
    ) -> Result<Self> {
        let mut stream = tokio::select! {
            _ = cancel.cancelled() => return Err(HarborError::Cancelled),
            result = client::open_tls(host, port, expected_fingerprint) => result?,
        };

        let hello = Request::DataHello {
            token: token.to_string(),
        };
        tokio::select! {
            _ = cancel.cancelled() => return Err(HarborError::Cancelled),
            result = tokio::time::timeout(DATA_HANDSHAKE_TIMEOUT, write_msg(&mut stream, &hello)) => {
                result.map_err(|_| HarborError::Timeout)??;
            }
        }
        let response: Response = tokio::select! {
            _ = cancel.cancelled() => return Err(HarborError::Cancelled),
            result = tokio::time::timeout(DATA_HANDSHAKE_TIMEOUT, read_msg(&mut stream)) => {
                result.map_err(|_| HarborError::Timeout)??
            }
        };
        match response {
            Response::DataReady => Ok(Self { stream, cancel }),
            Response::Error { code, message } => Err(map_data_error(&code, &message)),
            _ => Err(HarborError::Protocol("数据连接握手响应异常".into())),
        }
    }

    /// 打开一个源端文件并拉取完整块清单。
    pub async fn open_file(&mut self, share_id: &str, path: &str) -> Result<RemoteFileHeader> {
        self.send(&DataRequest::FileOpen {
            share_id: share_id.to_string(),
            path: path.to_string(),
        })
        .await?;

        let (mut header, chunk_count) = match self.recv().await? {
            DataResponse::FileHeader {
                size,
                mtime_ms,
                mode,
                content_hash,
                chunk_count,
                stable,
            } => {
                if !stable {
                    return Err(HarborError::Other("源文件在读取过程中发生变化".into()));
                }
                if chunk_count > MAX_OPEN_FILE_CHUNKS {
                    return Err(HarborError::Protocol("源端块数超过上限".into()));
                }
                (
                    RemoteFileHeader {
                        size,
                        mtime_ms,
                        mode,
                        content_hash,
                        chunks: Vec::with_capacity(chunk_count as usize),
                        stable,
                    },
                    chunk_count,
                )
            }
            DataResponse::Error { code, message } => return Err(map_data_error(&code, &message)),
            _ => return Err(HarborError::Protocol("文件头响应异常".into())),
        };

        let mut start = 0u32;
        while start < chunk_count {
            self.send(&DataRequest::ChunkMeta {
                start,
                count: crate::sync::CHUNK_META_PAGE_SIZE,
            })
            .await?;
            match self.recv().await? {
                DataResponse::ChunkMeta {
                    start: reply_start,
                    chunks,
                } => {
                    if reply_start != start || chunks.is_empty() {
                        return Err(HarborError::Protocol("块清单分页异常".into()));
                    }
                    header.chunks.extend(chunks);
                    start = header.chunks.len() as u32;
                }
                DataResponse::Error { code, message } => {
                    return Err(map_data_error(&code, &message))
                }
                _ => return Err(HarborError::Protocol("块清单响应异常".into())),
            }
        }

        if header.chunks.len() as u32 != chunk_count {
            return Err(HarborError::Protocol("块清单数量不一致".into()));
        }
        let sum: u64 = header.chunks.iter().map(|chunk| chunk.len as u64).sum();
        if sum != header.size {
            return Err(HarborError::Protocol("块长度之和与文件大小不一致".into()));
        }
        Ok(header)
    }

    /// 请求一批块；返回顺序与源端发送顺序一致。
    pub async fn request_chunks(&mut self, indices: &[u32]) -> Result<Vec<(u32, Vec<u8>)>> {
        if indices.is_empty() {
            return Ok(Vec::new());
        }
        if indices.len() > 128 {
            return Err(HarborError::Protocol("单次请求块数过多".into()));
        }
        let requested: HashSet<u32> = indices.iter().copied().collect();
        if requested.len() != indices.len() {
            return Err(HarborError::Protocol("块请求包含重复索引".into()));
        }

        self.send(&DataRequest::ChunkRequest {
            indices: indices.to_vec(),
        })
        .await?;

        let mut out = Vec::with_capacity(indices.len());
        loop {
            match self.recv_frame().await? {
                Frame::Chunk { index, data } => {
                    if !requested.contains(&index) {
                        return Err(HarborError::Protocol("源端发送了未请求的块".into()));
                    }
                    if out.iter().any(|(seen, _)| *seen == index) {
                        return Err(HarborError::Protocol("源端重复发送了同一块".into()));
                    }
                    out.push((index, data));
                }
                Frame::Json(bytes) => {
                    let response: DataResponse = serde_json::from_slice(&bytes)?;
                    match response {
                        DataResponse::ChunksDone { count } => {
                            if count as usize != out.len() {
                                return Err(HarborError::Protocol("块数量与请求不一致".into()));
                            }
                            return Ok(out);
                        }
                        DataResponse::Error { code, message } => {
                            return Err(map_data_error(&code, &message))
                        }
                        _ => return Err(HarborError::Protocol("块请求响应异常".into())),
                    }
                }
            }
        }
    }

    pub async fn close_file(&mut self) -> Result<()> {
        self.send(&DataRequest::FileClose).await?;
        match self.recv().await? {
            DataResponse::Closed => Ok(()),
            DataResponse::Error { code, message } => Err(map_data_error(&code, &message)),
            _ => Err(HarborError::Protocol("关闭文件响应异常".into())),
        }
    }

    async fn send(&mut self, request: &DataRequest) -> Result<()> {
        tokio::select! {
            _ = self.cancel.cancelled() => Err(HarborError::Cancelled),
            result = tokio::time::timeout(DATA_OP_TIMEOUT, write_frame_json(&mut self.stream, request)) => {
                result.map_err(|_| HarborError::Timeout)?
            }
        }
    }

    async fn recv(&mut self) -> Result<DataResponse> {
        match self.recv_frame().await? {
            Frame::Json(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Frame::Chunk { .. } => Err(HarborError::Protocol("此处不应出现文件块".into())),
        }
    }

    async fn recv_frame(&mut self) -> Result<Frame> {
        tokio::select! {
            _ = self.cancel.cancelled() => Err(HarborError::Cancelled),
            result = tokio::time::timeout(DATA_OP_TIMEOUT, read_frame(&mut self.stream)) => {
                result.map_err(|_| HarborError::Timeout)?
            }
        }
    }
}

fn map_data_error(code: &str, message: &str) -> HarborError {
    match code {
        "invalid_path" => HarborError::InvalidPath,
        "not_found" => HarborError::NotFound(message.to_string()),
        "cancelled" => HarborError::Cancelled,
        "timeout" => HarborError::Timeout,
        _ => HarborError::Other(message.to_string()),
    }
}

/// 把数据连接错误区分为「可继续下一个文件」与「网络已断」。
pub fn is_network_error(error: &HarborError) -> bool {
    matches!(
        error,
        HarborError::Io(_) | HarborError::Timeout | HarborError::Cancelled
    )
}
