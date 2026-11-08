use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock as StdRwLock};

use serde::Serialize;

use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, oneshot, Mutex, RwLock};
use tokio::time::{timeout, Duration};
use tokio_rustls::client::TlsStream;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::client;
use crate::config::{Config, ShareConfig};
use crate::discovery::{Advertiser, Browser};
use crate::error::{HarborError, Result};
use crate::identity::Identity;
use crate::pairing::PairingCode;
use crate::protocol::{read_msg, write_msg, Request, Response};
use crate::server::{self, ServiceHandle};
use crate::shares::{self, ShareStats};
use crate::types::{
    short_fingerprint, ConnectedClient, CoreEvent, DeviceInfo, DiscoveredSource, RemoteShare,
    RemoteSourceInfo, ShareSummary, SourceInfo,
};
use crate::util::{self, addresses_with_port, now_ms, os_label};
use crate::{APP_VERSION, DEFAULT_PORT};

#[derive(Debug, Clone, Serialize)]
pub struct PendingApproval {
    pub request_id: String,
    pub device: DeviceInfo,
    pub address: String,
    pub sas: String,
    pub created_at: u64,
}

pub struct RemoteSession {
    pub info: RemoteSourceInfo,
    pub conn: Mutex<TlsStream<TcpStream>>,
}

pub struct Engine {
    pub data_dir: PathBuf,
    pub identity: Identity,
    pub config: RwLock<Config>,
    local_device: StdRwLock<DeviceInfo>,
    stats: RwLock<HashMap<String, ShareStats>>,
    service: Mutex<Option<ServiceHandle>>,
    code: RwLock<PairingCode>,
    approvals: Mutex<HashMap<String, oneshot::Sender<bool>>>,
    pending: RwLock<HashMap<String, PendingApproval>>,
    clients: RwLock<Vec<ConnectedClient>>,
    sessions: Mutex<HashMap<String, Arc<RemoteSession>>>,
    connects: Mutex<HashMap<String, CancellationToken>>,
    discovery: Mutex<Option<Browser>>,
    discovered: Arc<StdRwLock<HashMap<String, DiscoveredSource>>>,
    pub events: broadcast::Sender<CoreEvent>,
}

impl Engine {
    pub fn new(data_dir: PathBuf) -> Result<Arc<Self>> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        std::fs::create_dir_all(&data_dir)
            .map_err(|e| HarborError::Storage(format!("创建数据目录失败：{e}")))?;
        let identity = Identity::load_or_create(&data_dir.join("identity"))?;
        let config = Config::load(&data_dir);
        let device = DeviceInfo {
            id: identity.device_id.clone(),
            name: config.device_name.clone(),
            os: os_label(),
            app_version: APP_VERSION.to_string(),
        };
        let (events, _) = broadcast::channel(256);
        Ok(Arc::new(Self {
            data_dir,
            identity,
            config: RwLock::new(config),
            local_device: StdRwLock::new(device),
            stats: RwLock::new(HashMap::new()),
            service: Mutex::new(None),
            code: RwLock::new(PairingCode::new()),
            approvals: Mutex::new(HashMap::new()),
            pending: RwLock::new(HashMap::new()),
            clients: RwLock::new(Vec::new()),
            sessions: Mutex::new(HashMap::new()),
            connects: Mutex::new(HashMap::new()),
            discovery: Mutex::new(None),
            discovered: Arc::new(StdRwLock::new(HashMap::new())),
            events,
        }))
    }

    pub fn subscribe(&self) -> broadcast::Receiver<CoreEvent> {
        self.events.subscribe()
    }

    pub fn device_info(&self) -> DeviceInfo {
        self.local_device.read().unwrap().clone()
    }

    pub async fn set_device_name(&self, name: &str) -> Result<DeviceInfo> {
        let name = name.trim();
        if name.is_empty() {
            return Err(HarborError::InvalidDeviceName);
        }
        if name.chars().count() > 64 {
            return Err(HarborError::InvalidDeviceName);
        }
        {
            let mut cfg = self.config.write().await;
            cfg.device_name = name.to_string();
            cfg.save(&self.data_dir)?;
        }
        let mut device = self.local_device.write().unwrap();
        device.name = name.to_string();
        Ok(device.clone())
    }

    pub async fn source_info(&self) -> SourceInfo {
        let config = self.config.read().await.clone();
        let service = self.service.lock().await;
        let running = service.is_some();
        let port = service.as_ref().map(|s| s.port).unwrap_or_else(|| {
            if config.port == 0 {
                DEFAULT_PORT
            } else {
                config.port
            }
        });
        drop(service);

        let (code, expires) = {
            let guard = self.code.read().await;
            if running && !guard.expired() {
                (guard.code().to_string(), guard.seconds_left())
            } else {
                (String::new(), 0)
            }
        };

        let stats = self.stats.read().await;
        let shares = config
            .shares
            .iter()
            .map(|s| {
                let st = stats.get(&s.id).copied().unwrap_or_default();
                ShareSummary {
                    id: s.id.clone(),
                    name: s.name.clone(),
                    path: util::display_path(&s.path),
                    enabled: s.enabled,
                    file_count: st.files,
                    total_bytes: st.bytes,
                }
            })
            .collect();

        SourceInfo {
            running,
            device: self.device_info(),
            port,
            addresses: addresses_with_port(port),
            fingerprint: self.identity.fingerprint.clone(),
            code,
            code_expires_in: expires,
            shares,
            clients: self.clients.read().await.clone(),
        }
    }

    pub async fn start_source(self: &Arc<Self>) -> Result<SourceInfo> {
        if self.service.lock().await.is_some() {
            return Ok(self.source_info().await);
        }

        let config = self.config.read().await.clone();
        let listener = match TcpListener::bind(("0.0.0.0", config.port)).await {
            Ok(listener) => listener,
            Err(err) => {
                tracing::warn!(?err, port = config.port, "默认端口不可用，改用随机端口");
                TcpListener::bind(("0.0.0.0", 0)).await?
            }
        };
        let port = listener.local_addr()?.port();
        *self.code.write().await = PairingCode::new();

        let info = self.source_info().await;
        let advertiser = if std::env::var("HARBOR_DISABLE_MDNS").is_ok() {
            None
        } else {
            match Advertiser::start(&info, port) {
                Ok(adv) => Some(adv),
                Err(err) => {
                    tracing::warn!(?err, "mDNS 广播失败，仍可通过手动地址连接");
                    None
                }
            }
        };

        let handle = server::spawn(self.clone(), listener, port, advertiser).await?;
        *self.service.lock().await = Some(handle);

        let engine = self.clone();
        tokio::spawn(async move { engine.refresh_stats().await });

        let info = self.source_info().await;
        let _ = self
            .events
            .send(CoreEvent::ServiceStarted { info: info.clone() });
        Ok(info)
    }

    pub async fn stop_source(&self) {
        let handle = self.service.lock().await.take();
        let had_service = handle.is_some();
        if let Some(handle) = handle {
            handle.cancel.cancel();
        }
        {
            let mut approvals = self.approvals.lock().await;
            for (_, tx) in approvals.drain() {
                let _ = tx.send(false);
            }
        }
        self.pending.write().await.clear();
        self.clients.write().await.clear();
        if had_service {
            let _ = self.events.send(CoreEvent::ServiceStopped);
        }
    }

    pub async fn rotate_code(&self) -> SourceInfo {
        *self.code.write().await = PairingCode::new();
        self.source_info().await
    }

    pub async fn add_share(&self, input: &str) -> Result<ShareSummary> {
        let path = shares::validate_share_dir(input)?;
        let name = shares::display_name(&path);
        let path_str = path.to_string_lossy().to_string();

        {
            let cfg = self.config.read().await;
            if cfg.shares.len() >= 32 {
                return Err(HarborError::Conflict("最多同时共享 32 个文件夹".into()));
            }
            for existing in &cfg.shares {
                let existing_path = PathBuf::from(&existing.path);
                if path == existing_path
                    || path.starts_with(&existing_path)
                    || existing_path.starts_with(&path)
                {
                    return Err(HarborError::Conflict(format!(
                        "「{}」与已有共享目录重叠",
                        existing.name
                    )));
                }
            }
        }

        let id = Uuid::new_v4()
            .to_string()
            .chars()
            .take(8)
            .collect::<String>();
        let entry = ShareConfig {
            id: id.clone(),
            name: name.clone(),
            path: path_str,
            enabled: true,
        };
        {
            let mut cfg = self.config.write().await;
            cfg.shares.push(entry);
            cfg.save(&self.data_dir)?;
        }
        let stats_path = path.clone();
        let stats = tokio::task::spawn_blocking(move || shares::scan_stats(&stats_path))
            .await
            .unwrap_or_default();
        self.stats.write().await.insert(id.clone(), stats);

        Ok(ShareSummary {
            id,
            name,
            path: util::display_path(&path.to_string_lossy()),
            enabled: true,
            file_count: stats.files,
            total_bytes: stats.bytes,
        })
    }

    pub async fn remove_share(&self, id: &str) -> Result<()> {
        {
            let mut cfg = self.config.write().await;
            let before = cfg.shares.len();
            cfg.shares.retain(|s| s.id != id);
            if cfg.shares.len() == before {
                return Err(HarborError::NotFound(id.into()));
            }
            cfg.save(&self.data_dir)?;
        }
        self.stats.write().await.remove(id);
        Ok(())
    }

    pub async fn set_share_enabled(&self, id: &str, enabled: bool) -> Result<()> {
        let mut cfg = self.config.write().await;
        let share = cfg
            .shares
            .iter_mut()
            .find(|s| s.id == id)
            .ok_or_else(|| HarborError::NotFound(id.into()))?;
        share.enabled = enabled;
        cfg.save(&self.data_dir)
    }

    pub async fn refresh_stats(&self) {
        let shares = self.config.read().await.shares.clone();
        for share in shares {
            let path = PathBuf::from(&share.path);
            let id = share.id;
            let stats = tokio::task::spawn_blocking(move || shares::scan_stats(&path))
                .await
                .unwrap_or_default();
            self.stats.write().await.insert(id, stats);
        }
    }

    /// 校验一次连接码；成功后立即作废并换新码（单次使用）。
    pub async fn try_authenticate(&self, input: &str) -> bool {
        let mut guard = self.code.write().await;
        if guard.verify(input) {
            guard.consume();
            *guard = PairingCode::new();
            true
        } else {
            false
        }
    }

    pub async fn add_pending(&self, approval: PendingApproval, tx: oneshot::Sender<bool>) {
        let id = approval.request_id.clone();
        self.pending.write().await.insert(id.clone(), approval);
        self.approvals.lock().await.insert(id, tx);
    }

    pub async fn remove_pending(&self, id: &str) {
        self.pending.write().await.remove(id);
        self.approvals.lock().await.remove(id);
    }

    pub async fn approve(&self, request_id: &str, allow: bool) -> Result<()> {
        let tx = self.approvals.lock().await.remove(request_id);
        self.pending.write().await.remove(request_id);
        match tx {
            Some(tx) => {
                let _ = tx.send(allow);
                Ok(())
            }
            None => Err(HarborError::NotFound(request_id.into())),
        }
    }

    pub async fn pending_requests(&self) -> Vec<PendingApproval> {
        let map = self.pending.read().await;
        let mut list: Vec<&PendingApproval> = map.values().collect();
        list.sort_by_key(|p| p.created_at);
        list.into_iter()
            .map(|p| PendingApproval {
                request_id: p.request_id.clone(),
                device: p.device.clone(),
                address: p.address.clone(),
                sas: p.sas.clone(),
                created_at: p.created_at,
            })
            .collect()
    }

    pub async fn add_client(&self, device: DeviceInfo, address: String) {
        let mut clients = self.clients.write().await;
        clients.retain(|c| c.device.id != device.id);
        clients.push(ConnectedClient {
            device,
            address,
            connected_at: now_ms(),
        });
    }

    pub async fn remove_client(&self, device_id: &str) {
        let mut clients = self.clients.write().await;
        clients.retain(|c| c.device.id != device_id);
    }

    pub async fn remote_shares(&self) -> Vec<RemoteShare> {
        let cfg = self.config.read().await;
        let stats = self.stats.read().await;
        cfg.shares
            .iter()
            .filter(|s| s.enabled)
            .map(|s| {
                let st = stats.get(&s.id).copied().unwrap_or_default();
                RemoteShare {
                    id: s.id.clone(),
                    name: s.name.clone(),
                    file_count: st.files,
                    total_bytes: st.bytes,
                }
            })
            .collect()
    }

    pub async fn browse_share(
        &self,
        share_id: &str,
        rel: &str,
    ) -> Result<(String, Vec<crate::types::Entry>)> {
        let root = {
            let cfg = self.config.read().await;
            let share = cfg
                .shares
                .iter()
                .find(|s| s.id == share_id)
                .ok_or_else(|| HarborError::NotFound(share_id.into()))?;
            if !share.enabled {
                return Err(HarborError::NotFound(share_id.into()));
            }
            PathBuf::from(&share.path)
        };
        let rel = rel.to_string();
        let list_path = rel.clone();
        let entries = tokio::task::spawn_blocking(move || shares::list_dir(&root, &list_path))
            .await
            .map_err(|e| HarborError::Other(format!("读取目录失败：{e}")))??;
        Ok((rel, entries))
    }

    pub async fn start_discovery(self: &Arc<Self>) -> Result<()> {
        let mut guard = self.discovery.lock().await;
        if guard.is_some() {
            return Ok(());
        }
        let browser = Browser::start(
            self.identity.device_id.clone(),
            self.events.clone(),
            self.discovered.clone(),
        )?;
        *guard = Some(browser);
        Ok(())
    }

    pub async fn stop_discovery(&self) {
        let browser = self.discovery.lock().await.take();
        if let Some(browser) = browser {
            browser.stop().await;
        }
        self.discovered.write().unwrap().clear();
    }

    pub fn discovered_sources(&self) -> Vec<DiscoveredSource> {
        let map = self.discovered.read().unwrap();
        let mut list: Vec<DiscoveredSource> = map.values().cloned().collect();
        list.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        list
    }

    pub async fn start_connect(
        self: &Arc<Self>,
        host: String,
        port: u16,
        code: String,
        expected_fingerprint: Option<String>,
    ) -> String {
        let connect_id = Uuid::new_v4().to_string();
        let cancel = CancellationToken::new();
        self.connects
            .lock()
            .await
            .insert(connect_id.clone(), cancel.clone());
        let engine = self.clone();
        let id = connect_id.clone();
        tokio::spawn(async move {
            client::connect(
                engine.clone(),
                id.clone(),
                host,
                port,
                code,
                expected_fingerprint,
                cancel,
            )
            .await;
            engine.connects.lock().await.remove(&id);
        });
        connect_id
    }

    pub async fn cancel_connect(&self, connect_id: &str) {
        if let Some(token) = self.connects.lock().await.remove(connect_id) {
            token.cancel();
        }
    }

    pub(crate) async fn insert_session(&self, id: String, session: Arc<RemoteSession>) {
        self.sessions.lock().await.insert(id, session);
    }

    async fn session(&self, id: &str) -> Result<Arc<RemoteSession>> {
        self.sessions
            .lock()
            .await
            .get(id)
            .cloned()
            .ok_or_else(|| HarborError::NotFound(id.into()))
    }

    pub async fn disconnect_remote(&self, session_id: &str) {
        self.sessions.lock().await.remove(session_id);
    }

    pub async fn remote_list_shares(&self, session_id: &str) -> Result<Vec<RemoteShare>> {
        let session = self.session(session_id).await?;
        let mut conn = session.conn.lock().await;
        write_msg(&mut *conn, &Request::ListShares).await?;
        let reply: Response = timeout(Duration::from_secs(15), read_msg(&mut *conn))
            .await
            .map_err(|_| HarborError::Timeout)??;
        match reply {
            Response::Shares { shares } => Ok(shares),
            Response::Error { code, message } => Err(map_remote_error(&code, &message)),
            _ => Err(HarborError::Protocol("意外的响应".into())),
        }
    }

    pub async fn remote_browse(
        &self,
        session_id: &str,
        share_id: &str,
        path: &str,
    ) -> Result<(String, Vec<crate::types::Entry>)> {
        let session = self.session(session_id).await?;
        let mut conn = session.conn.lock().await;
        write_msg(
            &mut *conn,
            &Request::Browse {
                share_id: share_id.to_string(),
                path: path.to_string(),
            },
        )
        .await?;
        let reply: Response = timeout(Duration::from_secs(15), read_msg(&mut *conn))
            .await
            .map_err(|_| HarborError::Timeout)??;
        match reply {
            Response::Browse { path, entries } => Ok((path, entries)),
            Response::Error { code, message } => Err(map_remote_error(&code, &message)),
            _ => Err(HarborError::Protocol("意外的响应".into())),
        }
    }

    pub fn default_destination(&self) -> String {
        util::default_save_root().to_string_lossy().to_string()
    }

    /// 测试与 CLI 使用：无 UI 时自动批准。
    pub async fn auto_approve_all(&self) {
        let ids: Vec<String> = self.pending.read().await.keys().cloned().collect();
        for id in ids {
            let _ = self.approve(&id, true).await;
        }
    }

    pub fn fingerprint_short(&self) -> String {
        short_fingerprint(&self.identity.fingerprint)
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }
}

fn map_remote_error(code: &str, message: &str) -> HarborError {
    match code {
        "auth_failed" => HarborError::AuthFailed,
        "denied" => HarborError::Denied,
        "invalid_path" => HarborError::InvalidPath,
        "not_found" => HarborError::NotFound(message.to_string()),
        _ => HarborError::Other(message.to_string()),
    }
}
