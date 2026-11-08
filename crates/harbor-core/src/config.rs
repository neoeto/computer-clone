use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{HarborError, Result};
use crate::util::default_device_name;
use crate::DEFAULT_PORT;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShareConfig {
    pub id: String,
    pub name: String,
    pub path: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_device_name")]
    pub device_name: String,
    #[serde(default = "default_port_value")]
    pub port: u16,
    #[serde(default)]
    pub shares: Vec<ShareConfig>,
}

fn default_port_value() -> u16 {
    DEFAULT_PORT
}

impl Default for Config {
    fn default() -> Self {
        Self {
            device_name: default_device_name(),
            port: DEFAULT_PORT,
            shares: Vec::new(),
        }
    }
}

impl Config {
    pub fn load(dir: &Path) -> Self {
        let path = dir.join("config.json");
        match fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<Config>(&text) {
                Ok(mut cfg) => {
                    if cfg.device_name.trim().is_empty() {
                        cfg.device_name = default_device_name();
                    }
                    if cfg.port == 0 {
                        cfg.port = DEFAULT_PORT;
                    }
                    cfg
                }
                Err(err) => {
                    tracing::warn!(?err, "配置文件解析失败，使用默认配置");
                    Config::default()
                }
            },
            Err(_) => Config::default(),
        }
    }

    pub fn save(&self, dir: &Path) -> Result<()> {
        fs::create_dir_all(dir)
            .map_err(|e| HarborError::Storage(format!("创建配置目录失败：{e}")))?;
        let path = dir.join("config.json");
        let tmp = dir.join("config.json.tmp");
        let text = serde_json::to_string_pretty(self)?;
        fs::write(&tmp, text).map_err(|e| HarborError::Storage(format!("写入配置失败：{e}")))?;
        fs::rename(&tmp, &path).map_err(|e| HarborError::Storage(format!("替换配置失败：{e}")))?;
        Ok(())
    }

    pub fn share_paths(&self) -> Vec<PathBuf> {
        self.shares.iter().map(|s| PathBuf::from(&s.path)).collect()
    }
}
