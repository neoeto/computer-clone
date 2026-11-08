use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::error::{HarborError, Result};
use crate::types::short_fingerprint;
use crate::types::{CoreEvent, DiscoveredSource, SourceInfo};
use crate::util::{now_ms, primary_ipv4};
use crate::{APP_VERSION, SERVICE_TYPE};

/// 源端共享服务的 mDNS 广播。Drop 时注销，尽量不留下僵尸记录。
pub struct Advertiser {
    daemon: ServiceDaemon,
    fullname: String,
}

impl Advertiser {
    pub fn start(info: &SourceInfo, port: u16) -> Result<Self> {
        let daemon =
            ServiceDaemon::new().map_err(|e| HarborError::Other(format!("mDNS 启动失败：{e}")))?;
        let ip = primary_ipv4()
            .ok_or_else(|| HarborError::Other("没有可用的局域网 IPv4 地址".into()))?;
        let host = format!(
            "{}.local.",
            sanitize_host(&crate::util::default_device_name())
        );
        let instance = format!("Harbor-{}", &info.device.id[..8.min(info.device.id.len())]);

        let mut props: HashMap<String, String> = HashMap::new();
        props.insert("id".into(), info.device.id.clone());
        props.insert("name".into(), info.device.name.clone());
        props.insert("os".into(), info.device.os.clone());
        props.insert("ver".into(), APP_VERSION.into());
        props.insert("fp".into(), short_fingerprint(&info.fingerprint));

        let service = ServiceInfo::new(SERVICE_TYPE, &instance, &host, ip.to_string(), port, props)
            .map_err(|e| HarborError::Other(format!("mDNS 服务信息不合法：{e}")))?;
        let fullname = service.get_fullname().to_string();
        daemon
            .register(service)
            .map_err(|e| HarborError::Other(format!("mDNS 注册失败：{e}")))?;
        Ok(Self { daemon, fullname })
    }
}

impl Drop for Advertiser {
    fn drop(&mut self) {
        let _ = self.daemon.unregister(&self.fullname);
        let _ = self.daemon.shutdown();
    }
}

/// 目的端的 mDNS 浏览。持续写入共享的 `sources` 表，并推送事件。
pub struct Browser {
    daemon: ServiceDaemon,
    cancel: CancellationToken,
    task: JoinHandle<()>,
    pub sources: Arc<RwLock<HashMap<String, DiscoveredSource>>>,
}

impl Browser {
    pub fn start(
        own_device_id: String,
        events: broadcast::Sender<CoreEvent>,
        sources: Arc<RwLock<HashMap<String, DiscoveredSource>>>,
    ) -> Result<Self> {
        let daemon =
            ServiceDaemon::new().map_err(|e| HarborError::Other(format!("mDNS 启动失败：{e}")))?;
        let receiver = daemon
            .browse(SERVICE_TYPE)
            .map_err(|e| HarborError::Other(format!("mDNS 浏览失败：{e}")))?;
        let cancel = CancellationToken::new();
        let state = sources.clone();
        let token = cancel.clone();

        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = token.cancelled() => break,
                    event = receiver.recv_async() => {
                        let event = match event { Ok(e) => e, Err(_) => break };
                        let changed = match event {
                            ServiceEvent::ServiceResolved(info) => {
                                let props = &info.txt_properties;
                                let id = props.get_property_val_str("id").unwrap_or_default().to_string();
                                if id.is_empty() || id == own_device_id {
                                    false
                                } else {
                                    let name = props.get_property_val_str("name").unwrap_or("Harbor").to_string();
                                    let os = props.get_property_val_str("os").unwrap_or_default().to_string();
                                    let fp = props.get_property_val_str("fp").unwrap_or_default().to_string();
                                    let address = info.addresses.iter()
                                        .find(|ip| ip.is_ipv4())
                                        .or_else(|| info.addresses.iter().next())
                                        .map(|ip| ip.to_ip_addr().to_string())
                                        .unwrap_or_default();
                                    if address.is_empty() {
                                        false
                                    } else {
                                        let mut map = state.write().unwrap();
                                        map.insert(id.clone(), DiscoveredSource {
                                            device_id: id,
                                            name,
                                            os,
                                            host: info.host.clone(),
                                            address,
                                            port: info.port,
                                            fingerprint: fp,
                                            last_seen: now_ms(),
                                            fullname: info.fullname.clone(),
                                        });
                                        true
                                    }
                                }
                            }
                            ServiceEvent::ServiceRemoved(_, fullname) => {
                                let mut map = state.write().unwrap();
                                let before = map.len();
                                map.retain(|_, v| v.fullname != fullname);
                                map.len() != before
                            }
                            _ => false,
                        };
                        if changed {
                            let list = snapshot(&state);
                            let _ = events.send(CoreEvent::Discovered { sources: list });
                        }
                    }
                }
            }
        });

        Ok(Self {
            daemon,
            cancel,
            task,
            sources,
        })
    }

    pub fn snapshot(&self) -> Vec<DiscoveredSource> {
        snapshot(&self.sources)
    }

    pub async fn stop(self) {
        self.cancel.cancel();
        let _ = self.daemon.shutdown();
        self.task.abort();
    }
}

fn snapshot(state: &Arc<RwLock<HashMap<String, DiscoveredSource>>>) -> Vec<DiscoveredSource> {
    let map = state.read().unwrap();
    let mut list: Vec<DiscoveredSource> = map.values().cloned().collect();
    list.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    list
}

fn sanitize_host(name: &str) -> String {
    let mut out: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    while out.contains("--") {
        out = out.replace("--", "-");
    }
    let trimmed = out.trim_matches('-');
    if trimmed.is_empty() {
        "harbor".into()
    } else {
        trimmed.chars().take(48).collect()
    }
}
