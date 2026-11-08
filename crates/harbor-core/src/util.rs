use std::net::IpAddr;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn app_data_dir() -> PathBuf {
    if let Ok(custom) = std::env::var("HARBOR_DATA_DIR") {
        if !custom.trim().is_empty() {
            return PathBuf::from(custom);
        }
    }
    dirs::data_dir()
        .map(|d| d.join("Harbor"))
        .unwrap_or_else(|| PathBuf::from(".harbor"))
}

pub fn default_device_name() -> String {
    let raw = hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| "Harbor".to_string());
    let trimmed = raw.trim().trim_end_matches(".local").trim();
    if trimmed.is_empty() {
        "我的电脑".to_string()
    } else {
        trimmed.to_string()
    }
}

pub fn os_label() -> String {
    match std::env::consts::OS {
        "macos" => "macOS",
        "windows" => "Windows",
        "linux" => "Linux",
        other => other,
    }
    .to_string()
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn is_usable_v4(ip: &std::net::Ipv4Addr) -> bool {
    !ip.is_loopback() && !ip.is_link_local() && !ip.is_unspecified() && !ip.is_broadcast()
}

fn is_private_v4(ip: &std::net::Ipv4Addr) -> bool {
    ip.is_private() || ip.is_loopback()
}

/// 枚举本机 IPv4，私有网段优先，去重。
pub fn local_ipv4s() -> Vec<std::net::Ipv4Addr> {
    let mut addrs: Vec<std::net::Ipv4Addr> = local_ip_address::list_afinet_netifas()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(_, ip)| match ip {
            IpAddr::V4(v4) if is_usable_v4(&v4) => Some(v4),
            _ => None,
        })
        .collect();
    addrs.sort_by_key(|ip| (!is_private_v4(ip), ip.octets()));
    addrs.dedup();
    addrs
}

pub fn primary_ipv4() -> Option<std::net::Ipv4Addr> {
    local_ipv4s().into_iter().next()
}

/// 展示给用户的 `ip:port` 列表。
pub fn addresses_with_port(port: u16) -> Vec<String> {
    let mut out: Vec<String> = local_ipv4s()
        .into_iter()
        .map(|ip| format!("{ip}:{port}"))
        .collect();
    if out.is_empty() {
        out.push(format!("127.0.0.1:{port}"));
    }
    out
}

pub fn expand_tilde(input: &str) -> PathBuf {
    let input = input.trim();
    if let Some(rest) = input.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    } else if input == "~" {
        if let Some(home) = dirs::home_dir() {
            return home;
        }
    }
    PathBuf::from(input)
}

/// 把绝对路径缩成 `~/...` 方便展示；仅用于本机 UI。
pub fn display_path(path: &str) -> String {
    if let Some(home) = dirs::home_dir() {
        let home_str = home.to_string_lossy();
        if let Some(rest) = path.strip_prefix(home_str.as_ref()) {
            if rest.is_empty() {
                return "~".into();
            }
            return format!("~{rest}");
        }
    }
    path.to_string()
}

pub fn default_save_root() -> PathBuf {
    dirs::home_dir()
        .map(|h| h.join("Harbor"))
        .unwrap_or_else(|| PathBuf::from("Harbor"))
}
