/// 配置管理模块
/// 负责读取、写入和管理应用配置
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

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

/// 配置目录固定为用户级 XDG 配置目录，与启动时的工作目录无关。
///
/// 早期版本会优先读取当前目录下的 `.config/config.json`，于是「在项目目录里启动」
/// 和「开机自启（WorkingDirectory=$HOME）」会落到两份不同的配置上：用户在界面里
/// 选中的设备只写进其中一份，换一种方式启动就被另一份覆盖。监控设备只能由用户的
/// 选择决定，因此这里必须只有一个事实来源。
///
/// 2026-11 项目改名为 OwlBTLock，目录随之从 `bt-proximity-lock` 改为 `owlbtlock`；
/// 旧目录下的 config.json 由 `migrate_legacy_config()` 一次性搬过来，用户选中的设备不丢。
fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("owlbtlock")
}

fn config_file() -> PathBuf {
    config_dir().join("config.json")
}

/// 一次性迁移旧配置：先找改名前的用户级目录，再找更早的项目内 `.config/`。
/// 只在新配置尚不存在时执行，绝不覆盖用户后来在界面上做过的选择。
fn migrate_legacy_config() {
    let target = config_file();
    if target.exists() {
        return;
    }
    let mut sources: Vec<PathBuf> = Vec::new();
    // 1. 改名前（bt-proximity-lock）的用户级配置 —— 里面是用户选中的监控设备，优先保留
    if let Some(base) = dirs::config_dir() {
        sources.push(base.join("bt-proximity-lock").join("config.json"));
    }
    // 2. 更早版本写在项目目录 .config/config.json
    sources.push(PathBuf::from(".config").join("config.json"));

    let Some(legacy) = sources.into_iter().find(|p| p.exists()) else {
        return;
    };
    if let Some(parent) = target.parent() {
        if let Err(e) = fs::create_dir_all(parent) {
            log::warn!("迁移旧配置失败（创建目录）: {e}");
            return;
        }
    }
    match fs::read_to_string(&legacy) {
        Ok(content) => match fs::write(&target, content) {
            Ok(_) => log::info!(
                "已迁移旧配置: {} -> {}（旧文件保留）",
                legacy.display(),
                target.display()
            ),
            Err(e) => log::warn!("迁移旧配置失败（写入）: {e}"),
        },
        Err(e) => log::warn!("迁移旧配置失败（读取）: {e}"),
    }
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
    /// 读取唯一的用户级配置；必要时先迁移旧版遗留配置。
    pub fn load() -> Self {
        migrate_legacy_config();
        let path = config_file();
        log::info!("读取配置: {}", path.display());
        Self::load_from(&path)
    }

    /// 从指定路径读取（可注入路径，便于验证「选择 → 重启」的一致性）。
    /// 文件不存在或解析失败时回退默认值，缺失字段由 serde default 补齐。
    pub fn load_from(path: &Path) -> Self {
        if !path.exists() {
            return Self::default();
        }
        match fs::read_to_string(path) {
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
    }

    pub fn save(&self) {
        self.save_to(&config_file());
    }

    /// 写入指定路径：先写临时文件再原子替换，避免写入中途被打断留下半份配置，
    /// 导致用户选中的设备丢失。
    pub fn save_to(&self, path: &Path) {
        if let Some(dir) = path.parent() {
            if let Err(e) = fs::create_dir_all(dir) {
                log::error!("创建配置目录失败: {e}");
                return;
            }
        }
        let json = match serde_json::to_string_pretty(self) {
            Ok(json) => json,
            Err(e) => {
                log::error!("序列化配置失败: {e}");
                return;
            }
        };
        let tmp = path.with_extension("json.tmp");
        if let Err(e) = fs::write(&tmp, &json) {
            log::error!("保存配置失败: {e}");
            return;
        }
        if let Err(e) = fs::rename(&tmp, path) {
            log::error!("保存配置失败（替换）: {e}");
            let _ = fs::remove_file(&tmp);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_config_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "owlbtlock-test-{}-{name}.json",
            std::process::id()
        ))
    }

    /// 用户在界面里选中设备后，重启程序读到的必须还是同一台。
    #[test]
    fn selected_device_survives_restart() {
        let path = temp_config_path("selected");
        let _ = fs::remove_file(&path);

        let mut cfg = Config::default();
        cfg.phone_mac = "AA:BB:CC:DD:EE:FF".into();
        cfg.phone_name = "我的手机".into();
        cfg.lock_delay_sec = 17;
        cfg.save_to(&path);

        let loaded = Config::load_from(&path);
        assert_eq!(loaded.phone_mac, "AA:BB:CC:DD:EE:FF");
        assert_eq!(loaded.phone_name, "我的手机");
        assert_eq!(loaded.lock_delay_sec, 17);

        let _ = fs::remove_file(&path);
    }

    /// 换一台设备后，旧设备必须被彻底替换，而不是残留。
    #[test]
    fn changing_device_replaces_the_previous_one() {
        let path = temp_config_path("changed");
        let _ = fs::remove_file(&path);

        let mut cfg = Config::default();
        cfg.phone_mac = "11:11:11:11:11:11".into();
        cfg.save_to(&path);

        cfg.phone_mac = "22:22:22:22:22:22".into();
        cfg.phone_name = "新设备".into();
        cfg.save_to(&path);

        let loaded = Config::load_from(&path);
        assert_eq!(loaded.phone_mac, "22:22:22:22:22:22");
        assert_eq!(loaded.phone_name, "新设备");

        let _ = fs::remove_file(&path);
    }

    /// 老配置缺字段时不能把已有选择丢掉。
    #[test]
    fn partial_config_keeps_device_and_fills_defaults() {
        let path = temp_config_path("partial");
        fs::write(&path, r#"{"phone_mac":"11:22:33:44:55:66"}"#).unwrap();

        let loaded = Config::load_from(&path);
        assert_eq!(loaded.phone_mac, "11:22:33:44:55:66");
        assert_eq!(loaded.lock_delay_sec, DEFAULT_LOCK_DELAY_SEC);
        assert_eq!(loaded.rssi_threshold, DEFAULT_RSSI_THRESHOLD);
        assert!(loaded.auto_connect_device);

        let _ = fs::remove_file(&path);
    }

    /// 配置路径不能随启动目录变化，否则从项目目录启动与开机自启会读到不同设备。
    #[test]
    fn config_dir_is_independent_of_working_directory() {
        let before = config_dir();
        let old_cwd = std::env::current_dir().unwrap();
        std::env::set_current_dir(std::env::temp_dir()).unwrap();
        let after = config_dir();
        std::env::set_current_dir(&old_cwd).unwrap();

        assert_eq!(before, after);
        assert!(after.is_absolute());
        assert!(after.ends_with("owlbtlock"));
    }
}
