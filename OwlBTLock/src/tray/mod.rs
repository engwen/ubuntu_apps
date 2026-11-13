/// 系统托盘模块
/// 抽象接口 + 多桌面环境适配
///
/// 已适配桌面环境：
/// - KDE Plasma    → StatusNotifierItem D-Bus
/// - Deepin/DDE    → StatusNotifierItem D-Bus
/// - XFCE          → StatusNotifierItem D-Bus
/// - Cinnamon      → StatusNotifierItem D-Bus
/// - MATE          → StatusNotifierItem D-Bus
/// - Unity         → StatusNotifierItem D-Bus
/// - GNOME         → 窗口隐藏回退（GNOME 不支持传统托盘）
/// - 其他          → 自动检测 StatusNotifierWatcher → 有则用 SNI，无则窗口隐藏
mod sni;
mod fallback;

use std::sync::mpsc;

/// 托盘动作
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayAction {
    Show,
    Quit,
}

/// 托盘状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayState {
    Connected,
    Fading,
    Disconnected,
    Paused,
    BleOff,
    Reconnecting,
}

/// 托盘后端 trait — 所有桌面环境实现此接口
pub trait TrayBackend: Send {
    /// 启动托盘服务
    fn start(&mut self) -> bool;
    /// 更新状态显示
    fn update_status(&mut self, state: TrayState, rssi: i32);
    /// 设置动作回调通道
    fn set_action_sender(&mut self, tx: mpsc::Sender<TrayAction>);
    /// 停止并清理
    fn stop(&mut self);
}

/// 托盘控制器 — 自动检测桌面环境并选择后端
pub struct TrayIcon {
    backend: Box<dyn TrayBackend>,
    action_rx: Option<mpsc::Receiver<TrayAction>>,
}

impl TrayIcon {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        let mut backend = detect_backend();
        backend.set_action_sender(tx);
        Self {
            backend,
            action_rx: Some(rx),
        }
    }

    pub fn start(&mut self) {
        self.backend.start();
    }

    pub fn update_status(&mut self, state: TrayState, rssi: i32) {
        self.backend.update_status(state, rssi);
    }

    /// 获取动作接收器（调用一次，所有权转移）
    pub fn take_action_receiver(&mut self) -> Option<mpsc::Receiver<TrayAction>> {
        self.action_rx.take()
    }

    pub fn stop(&mut self) {
        self.backend.stop();
    }
}

/// 检测桌面环境，选择最佳托盘后端
fn detect_backend() -> Box<dyn TrayBackend> {
    let desktop = std::env::var("XDG_CURRENT_DESKTOP")
        .unwrap_or_default()
        .to_lowercase();
    let session = std::env::var("XDG_SESSION_TYPE")
        .unwrap_or_default()
        .to_lowercase();

    log::info!("桌面环境: XDG_CURRENT_DESKTOP={}, XDG_SESSION_TYPE={}", desktop, session);

    // StatusNotifierItem 支持的桌面环境
    if desktop.contains("kde") || desktop.contains("plasma") {
        log::info!("检测到 KDE/Plasma，使用 StatusNotifierItem 托盘");
        return Box::new(sni::SnITray::new());
    }
    if desktop.contains("deepin") || desktop.contains("dde") {
        log::info!("检测到 Deepin/DDE，使用 StatusNotifierItem 托盘");
        return Box::new(sni::SnITray::new());
    }
    if desktop.contains("xfce") {
        log::info!("检测到 XFCE，使用 StatusNotifierItem 托盘");
        return Box::new(sni::SnITray::new());
    }
    if desktop.contains("cinnamon") {
        log::info!("检测到 Cinnamon，使用 StatusNotifierItem 托盘");
        return Box::new(sni::SnITray::new());
    }
    if desktop.contains("mate") {
        log::info!("检测到 MATE，使用 StatusNotifierItem 托盘");
        return Box::new(sni::SnITray::new());
    }
    if desktop.contains("unity") {
        log::info!("检测到 Unity，使用 StatusNotifierItem 托盘");
        return Box::new(sni::SnITray::new());
    }

    // 尝试检测系统是否支持 StatusNotifierItem
    if sni::watcher_exists() {
        log::info!("系统支持 StatusNotifierItem，使用 SNI 托盘");
        return Box::new(sni::SnITray::new());
    }

    // GNOME / 其他不支持传统托盘的环境
    log::info!("使用窗口隐藏回退方案（GNOME/其他）");
    Box::new(fallback::FallbackTray::new())
}
