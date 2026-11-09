/// 配置管理模块
/// 负责读取、写入和管理应用配置
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

// ── 默认值常量 ──────────────────────────────────────────────────────────────
pub const DEFAULT_LOCK_DELAY_SEC: i32 = 9;
pub const DEFAULT_RSSI_THRESHOLD: i32 = -50;
pub const DEFAULT_SCAN_INTERVAL_SEC: i32 = 3;
pub const DEFAULT_DISCONNECT_PERIPHERALS: bool = false;
pub const DEFAULT_AUTO_RESUME: bool = true;
pub const DEFAULT_AUTO_UNLOCK: bool = false;
pub const DEFAULT_AUTO_START: bool = false;
pub const DEFAULT_AUTO_LOCK: bool = true;
pub const DEFAULT_AUTO_CONNECT: bool = true;
pub const DEFAULT_WINDOW_WIDTH: i32 = 500;
pub const DEFAULT_WINDOW_HEIGHT: i32 = 900;

fn config_dir() -> PathBuf {
    // 优先使用项目目录下的 .config
    let project_config = PathBuf::from(".config");
    if project_config.exists() {
        return project_config;
    }
    // 回退到 ~/.config/bt-proximity-lock/
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("bt-proximity-lock")
}

fn config_file() -> PathBuf {
    config_dir().join("config.json")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub phone_mac: String,
    #[serde(default)]
    pub phone_name: String,
    #[serde(default = "default_lock_delay_sec")]
    pub lock_delay_sec: i32,
    #[serde(default = "default_rssi_threshold")]
    pub rssi_threshold: i32,
    #[serde(default)]
    pub disconnect_peripherals: bool,
    #[serde(default = "default_disconnect_delay")]
    pub disconnect_delay_sec: i32,
    #[serde(default)]
    pub peripheral_whitelist: Vec<String>,
    #[serde(default = "default_scan_interval")]
    pub scan_interval_sec: i32,
    #[serde(default = "default_auto_connect")]
    pub auto_connect_device: bool,
    #[serde(default = "default_auto_lock")]
    pub auto_lock_enabled: bool,
    #[serde(default = "default_auto_resume")]
    pub auto_resume_on_reconnect: bool,
    #[serde(default)]
    pub auto_unlock_on_reconnect: bool,
    #[serde(default)]
    pub auto_start_on_boot: bool,
    #[serde(default = "default_language")]
    pub language: String,
    #[serde(default = "default_window_width")]
    pub window_width: i32,
    #[serde(default = "default_window_height")]
    pub window_height: i32,
}

fn default_lock_delay_sec() -> i32 {
    DEFAULT_LOCK_DELAY_SEC
}
fn default_rssi_threshold() -> i32 {
    DEFAULT_RSSI_THRESHOLD
}
fn default_disconnect_delay() -> i32 {
    5
}
fn default_scan_interval() -> i32 {
    DEFAULT_SCAN_INTERVAL_SEC
}
fn default_auto_connect() -> bool {
    DEFAULT_AUTO_CONNECT
}
fn default_auto_lock() -> bool {
    DEFAULT_AUTO_LOCK
}
fn default_auto_resume() -> bool {
    DEFAULT_AUTO_RESUME
}
fn default_language() -> String {
    String::new()
}
fn default_window_width() -> i32 {
    DEFAULT_WINDOW_WIDTH
}
fn default_window_height() -> i32 {
    DEFAULT_WINDOW_HEIGHT
}

impl Default for Config {
    fn default() -> Self {
        Self {
            phone_mac: String::new(),
            phone_name: String::new(),
            lock_delay_sec: DEFAULT_LOCK_DELAY_SEC,
            rssi_threshold: DEFAULT_RSSI_THRESHOLD,
            disconnect_peripherals: DEFAULT_DISCONNECT_PERIPHERALS,
            disconnect_delay_sec: 5,
            peripheral_whitelist: Vec::new(),
            scan_interval_sec: DEFAULT_SCAN_INTERVAL_SEC,
            auto_connect_device: DEFAULT_AUTO_CONNECT,
            auto_lock_enabled: DEFAULT_AUTO_LOCK,
            auto_resume_on_reconnect: DEFAULT_AUTO_RESUME,
            auto_unlock_on_reconnect: DEFAULT_AUTO_UNLOCK,
            auto_start_on_boot: DEFAULT_AUTO_START,
            language: String::new(),
            window_width: DEFAULT_WINDOW_WIDTH,
            window_height: DEFAULT_WINDOW_HEIGHT,
        }
    }
}

impl Config {
    pub fn load() -> Self {
        let path = config_file();
        if path.exists() {
            match fs::read_to_string(&path) {
                Ok(content) => match serde_json::from_str::<Config>(&content) {
                    Ok(mut cfg) => {
                        cfg.window_width = cfg.window_width.max(DEFAULT_WINDOW_WIDTH);
                        cfg.window_height = cfg.window_height.max(DEFAULT_WINDOW_HEIGHT);
                        cfg
                    }
                    Err(e) => {
                        log::warn!("配置解析失败，使用默认值: {e}");
                        Self::default()
                    }
                },
                Err(e) => {
                    log::warn!("配置读取失败: {e}");
                    Self::default()
                }
            }
        } else {
            Self::default()
        }
    }

    pub fn save(&self) {
        let dir = config_dir();
        if let Err(e) = fs::create_dir_all(&dir) {
            log::error!("创建配置目录失败: {e}");
            return;
        }
        let path = config_file();
        match serde_json::to_string_pretty(self) {
            Ok(json) => {
                if let Err(e) = fs::write(&path, json) {
                    log::error!("保存配置失败: {e}");
                }
            }
            Err(e) => log::error!("序列化配置失败: {e}"),
        }
    }
}
