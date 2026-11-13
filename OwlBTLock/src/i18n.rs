/// 国际化模块：支持中文/英文，首次启动自动检测系统语言
use std::collections::HashMap;
use std::sync::Mutex;

// 所有翻译文本
fn zh_cn_strings() -> HashMap<&'static str, &'static str> {
    let mut m = HashMap::new();
    m.insert("title", "OwlBTLock");
    m.insert("subtitle", "蓝牙远离锁屏");
    m.insert("status_ready", "就绪");
    m.insert("pick_title", "选择你的手机蓝牙设备");
    m.insert("pick_hint", "已配对设备（绿色表示当前已连接）");
    m.insert(
        "pick_no_devices",
        "未发现已连接的蓝牙设备\n\n请先确保手机蓝牙已开启并连接到此电脑\n然后点击「刷新设备列表」",
    );
    m.insert("btn_refresh", "刷新设备列表");
    m.insert("btn_back", "返回");
    m.insert("btn_start", "开始监控");
    m.insert("btn_stop", "停止监控");
    m.insert("btn_resume", "恢复监控");
    m.insert("btn_change", "更换设备");
    m.insert("label_device", "设备");
    m.insert("label_signal", "信号");
    m.insert("label_settings", "设置");
    m.insert("conn_connected", "已连接");
    m.insert("conn_fading", "信号减弱");
    m.insert("conn_disconnected", "未连接");
    m.insert("conn_ble_off", "蓝牙已关闭");
    m.insert("conn_reconnecting", "正在重新连接...");
    m.insert("status_scanning", "正在扫描已连接蓝牙设备...");
    m.insert("status_found", "发现 {n} 个已连接设备，请选择你的手机");
    m.insert("status_selected", "已选择: {name}，监控已自动启动");
    m.insert("status_init_fail", "蓝牙初始化失败，等待适配器就绪...");
    m.insert("status_monitoring_on", "监控已启动");
    m.insert("status_monitoring_off", "监控已停止");
    m.insert("status_please_select", "请先选择设备");
    m.insert(
        "status_ble_turned_off",
        "蓝牙已被关闭（用户操作），不会自动锁屏",
    );
    m.insert("status_ble_turned_on", "蓝牙已开启，监控已恢复");
    m.insert("status_reconnecting", "正在自动重新连接设备...");
    m.insert("status_connecting_attempt", "正在尝试连接（第 {n} 次）...");
    m.insert(
        "status_active_no_lock",
        "手机不在周围，但你正在使用电脑，不锁屏，持续重连中...",
    );
    m.insert("status_user_activity", "检测到用户操作，已取消锁屏");
    m.insert("status_countdown", "信号丢失，{n}秒后锁屏...");
    m.insert("status_user_resumed", "检测到用户操作，跳过锁屏");
    m.insert("status_locking", "正在锁定屏幕...");
    m.insert("status_locked", "屏幕已锁定");
    m.insert(
        "status_paused_waiting",
        "屏幕已锁定，等待蓝牙重连后自动恢复监控",
    );
    m.insert("status_stopped_after_lock", "屏幕已锁定，监控已停止");
    m.insert("status_lock_failed", "锁屏失败，监控继续运行");
    m.insert(
        "status_signal_recovered",
        "信号恢复 ({rssi} dBm)，已取消锁屏",
    );
    m.insert("status_phone_connected", "手机已连接 - 信号: {rssi} dBm");
    m.insert("status_monitor_paused", "监控已暂停（托盘）");
    m.insert("status_monitor_resumed", "监控已恢复（托盘）");
    m.insert("status_window_hidden", "窗口已隐藏，可通过托盘图标恢复");
    m.insert("settings_lock_delay", "锁屏延迟");
    m.insert("settings_rssi_threshold", "信号阈值");
    m.insert("settings_scan_interval", "重连/轮询间隔");
    m.insert("settings_disconnect_peripherals", "离开时断开蓝牙外设");
    m.insert("settings_auto_resume", "锁屏后重连自动恢复监控");
    m.insert("settings_auto_unlock", "蓝牙靠近自动解锁");
    m.insert("settings_auto_start", "开机时自动启动");
    m.insert("settings_auto_reconnect", "自动重新连接设备");
    m.insert("settings_auto_lock", "蓝牙远离自动锁屏");
    m.insert("settings_language", "语言");
    m.insert("btn_test", "诊断");
    m.insert("btn_restore_defaults", "恢复默认");
    m.insert("label_default", "默认");
    m.insert("test_title", "诊断工具");
    m.insert("test_rssi_curve", "RSSI 信号曲线");
    m.insert("test_log", "运行日志");
    m.insert("unit_sec", "秒");
    m.insert("unit_dbm", "dBm");
    m.insert("lang_chinese", "中文");
    m.insert("lang_english", "English");
    m.insert("auto_lock_enabled", "蓝牙远离自动锁屏");
    m
}

fn en_us_strings() -> HashMap<&'static str, &'static str> {
    let mut m = HashMap::new();
    m.insert("title", "OwlBTLock");
    m.insert("subtitle", "Bluetooth Away Lock");
    m.insert("status_ready", "Ready");
    m.insert("pick_title", "Select Your Bluetooth Device");
    m.insert(
        "pick_hint",
        "Only shows currently connected Bluetooth devices",
    );
    m.insert("pick_no_devices", "No connected Bluetooth devices found\n\nPlease make sure your phone Bluetooth is on\nand connected to this computer,\nthen click \"Refresh Device List\"");
    m.insert("btn_refresh", "Refresh Devices");
    m.insert("btn_back", "Back");
    m.insert("btn_start", "Start Monitor");
    m.insert("btn_stop", "Stop Monitor");
    m.insert("btn_resume", "Resume Monitor");
    m.insert("btn_change", "Change");
    m.insert("label_device", "Device");
    m.insert("label_signal", "Signal");
    m.insert("label_settings", "Settings");
    m.insert("conn_connected", "Connected");
    m.insert("conn_fading", "Signal Weak");
    m.insert("conn_disconnected", "Disconnected");
    m.insert("conn_ble_off", "Bluetooth Off");
    m.insert("conn_reconnecting", "Reconnecting...");
    m.insert("status_scanning", "Scanning connected devices...");
    m.insert(
        "status_found",
        "Found {n} connected device(s), please select your phone",
    );
    m.insert("status_selected", "Selected: {name}, monitoring started");
    m.insert(
        "status_init_fail",
        "Bluetooth init failed, waiting for adapter...",
    );
    m.insert("status_monitoring_on", "Monitoring started");
    m.insert("status_monitoring_off", "Monitoring stopped");
    m.insert("status_please_select", "Please select a device first");
    m.insert(
        "status_ble_turned_off",
        "Bluetooth turned off (user action), no lock",
    );
    m.insert("status_ble_turned_on", "Bluetooth on, monitoring restored");
    m.insert("status_reconnecting", "Auto-reconnecting device...");
    m.insert("status_connecting_attempt", "Connecting attempt #{n}...");
    m.insert(
        "status_active_no_lock",
        "Phone not nearby, but you're using the PC — no lock, still reconnecting...",
    );
    m.insert(
        "status_user_activity",
        "User activity detected, lock cancelled",
    );
    m.insert("status_countdown", "Signal lost, locking in {n}s...");
    m.insert(
        "status_user_resumed",
        "User activity detected, skipping lock",
    );
    m.insert("status_locking", "Locking screen...");
    m.insert("status_locked", "Screen locked");
    m.insert(
        "status_paused_waiting",
        "Screen locked, waiting for Bluetooth reconnect",
    );
    m.insert(
        "status_stopped_after_lock",
        "Screen locked, monitoring stopped",
    );
    m.insert("status_lock_failed", "Lock failed, monitoring continues");
    m.insert(
        "status_signal_recovered",
        "Signal recovered ({rssi} dBm), lock cancelled",
    );
    m.insert(
        "status_phone_connected",
        "Phone connected - signal: {rssi} dBm",
    );
    m.insert("status_monitor_paused", "Monitoring paused (tray)");
    m.insert("status_monitor_resumed", "Monitoring resumed (tray)");
    m.insert(
        "status_window_hidden",
        "Window hidden, restore via tray icon",
    );
    m.insert("settings_lock_delay", "Lock Delay");
    m.insert("settings_rssi_threshold", "Signal Threshold");
    m.insert("settings_scan_interval", "Reconnect/Poll Interval");
    m.insert(
        "settings_disconnect_peripherals",
        "Disconnect peripherals on leave",
    );
    m.insert(
        "settings_auto_resume",
        "Auto-resume monitoring on reconnect",
    );
    m.insert("settings_auto_unlock", "Auto-unlock when near");
    m.insert("settings_auto_start", "Auto-start on boot");
    m.insert("settings_auto_reconnect", "Auto-reconnect device");
    m.insert("settings_auto_lock", "Auto-lock when away");
    m.insert("settings_language", "Language");
    m.insert("btn_test", "Diagnostics");
    m.insert("btn_restore_defaults", "Restore Defaults");
    m.insert("label_default", "Default");
    m.insert("test_title", "Diagnostics");
    m.insert("test_rssi_curve", "RSSI Signal Curve");
    m.insert("test_log", "Activity Log");
    m.insert("unit_sec", "s");
    m.insert("unit_dbm", "dBm");
    m.insert("lang_chinese", "中文");
    m.insert("lang_english", "English");
    m.insert("auto_lock_enabled", "Auto-lock when away");
    m
}

// 全局语言状态（用 OnceLock 避免外部依赖）
use std::sync::OnceLock;

static CURRENT_LANG: OnceLock<Mutex<String>> = OnceLock::new();
static LANG_STRINGS_ZH: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
static LANG_STRINGS_EN: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();

fn current_lang() -> &'static Mutex<String> {
    CURRENT_LANG.get_or_init(|| Mutex::new("zh-CN".into()))
}

fn get_strings() -> &'static HashMap<&'static str, &'static str> {
    let lang = current_lang().lock().unwrap();
    if lang.as_str() == "en-US" {
        LANG_STRINGS_EN.get_or_init(en_us_strings)
    } else {
        LANG_STRINGS_ZH.get_or_init(zh_cn_strings)
    }
}

pub fn set_lang(lang: &str) {
    *current_lang().lock().unwrap() = lang.to_string();
}

pub fn get_lang() -> String {
    current_lang().lock().unwrap().clone()
}

/// 翻译字符串，支持简单占位符 {name} 替换
pub fn tr(key: &str) -> String {
    let strings = get_strings();
    strings.get(key).unwrap_or(&key).to_string()
}

/// 带参数的翻译
pub fn tr_fmt(key: &str, args: &[(&str, &str)]) -> String {
    let mut result = tr(key);
    for (k, v) in args {
        result = result.replace(&format!("{{{k}}}"), v);
    }
    result
}

/// 检测系统语言
pub fn detect_language() -> String {
    if let Ok(lang) = std::env::var("LC_MESSAGES") {
        let code = lang.split('.').next().unwrap_or("").to_lowercase();
        if code.starts_with("zh") {
            return "zh-CN".into();
        }
        if code.starts_with("en") {
            return "en-US".into();
        }
    }
    if let Ok(lang) = std::env::var("LANG") {
        let code = lang.split('.').next().unwrap_or("").to_lowercase();
        if code.starts_with("zh") {
            return "zh-CN".into();
        }
        if code.starts_with("en") {
            return "en-US".into();
        }
    }
    "zh-CN".into()
}
