mod bluetooth_monitor;
/// OwlBTLock - 主控制器：状态机 + GTK4 界面 + 蓝牙监控 + 系统托盘
mod autostart;
mod config;
mod i18n;
mod locker;
mod log_capture;
mod poller;
mod scanner;
mod tray;
mod ui;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

use gtk::prelude::*;
use gtk::{self, glib, Application, ApplicationWindow, Box as GtkBox};

use bluetooth_monitor::{BluetoothMonitor, BtEvent};
use config::Config;
use i18n::{detect_language, set_lang, tr};
use poller::SmartPoller;
use tray::{TrayIcon, TrayAction, TrayState};

// ── 轮询线程消息类型 ────────────────────────────────────────────────────

/// 轮询线程 → 主线程 的消息
#[derive(Debug, Clone)]
enum PollMsg {
    /// 轮询状态更新（连接状态、RSSI）
    Status {
        connected: bool,
        rssi: i32,
        rssi_supported: bool,
        fast_poll: bool,
        countdown: i32,
    },
    /// 蓝牙事件（连接/断开/信号变化等）
    Event(BtEvent),
    /// 连接尝试结果
    ConnectResult {
        success: bool,
        attempt: i32,
    },
}

/// 主线程 → 轮询线程 的控制命令
#[derive(Debug, Clone)]
enum CtrlCmd {
    /// 更新配置（阈值、轮询间隔）
    UpdateConfig { threshold: i32, scan_interval: i32 },
    /// 执行连接尝试
    Connect,
    /// 停止轮询线程
    Stop,
}

// ── 常量 ──────────────────────────────────────────────────────────────────
const MAX_CONNECT_ATTEMPTS: i32 = 3;
const CONNECT_TIMEOUT_SEC: u64 = 5;
const CONNECT_ATTEMPT_INTERVAL_SEC: u32 = 5;
const RETRY_BASE_INTERVAL_SEC: u64 = 10;
const RETRY_MAX_INTERVAL_SEC: u64 = 60;

// ── 应用状态 ──────────────────────────────────────────────────────────────
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppState {
    Idle,
    Monitoring,
    Countdown,
    Locking,
    AutoPaused,
    BleTurnedOff,
}

// ── 应用核心状态（使用 Arc<Mutex> 以支持跨线程访问）────────────────────────
struct AppCore {
    config: Config,
    app_state: AppState,
    paused: bool,
    tray: TrayIcon,
    // 连接状态（UI 显示用）
    conn_status: String,
    conn_rssi: i32,
    // 蓝牙状态缓存（来自轮询线程）
    current_connected: bool,
    current_rssi: i32,
    current_rssi_supported: bool,
    ever_connected: bool,
    lock_abandoned: bool,
    // 自动连接
    auto_connecting: bool,
    auto_connect_attempts: i32,
    connecting: bool,
    // 倒计时
    countdown_started: bool,
    countdown_expired: bool,
    countdown_remaining: i32,
    // 平滑采样：事件驱动路径中连续低于阈值的次数
    low_rssi_consecutive: i32,
    // 重连
    retry_pending: bool,
    retry_round_count: i32,
    // 轮询线程通道
    poll_rx: Option<mpsc::Receiver<PollMsg>>,
    ctrl_tx: Option<mpsc::Sender<CtrlCmd>>,
}

type SharedCore = Arc<Mutex<AppCore>>;

fn new_core(config: Config) -> SharedCore {
    Arc::new(Mutex::new(AppCore {
        config,
        app_state: AppState::Idle,
        paused: false,
        tray: TrayIcon::new(),
        conn_status: "disconnected".into(),
        conn_rssi: -100,
        current_connected: false,
        current_rssi: -100,
        current_rssi_supported: false,
        ever_connected: false,
        lock_abandoned: false,
        auto_connecting: false,
        auto_connect_attempts: 0,
        connecting: false,
        countdown_started: false,
        countdown_expired: false,
        countdown_remaining: 0,
        low_rssi_consecutive: 0,
        retry_pending: false,
        retry_round_count: 0,
        poll_rx: None,
        ctrl_tx: None,
    }))
}

// ── AppCore 辅助方法 ─────────────────────────────────────────────────────

impl AppCore {
    fn set_state(&mut self, new_state: AppState) {
        let old = self.app_state;
        self.app_state = new_state;
        if old != new_state {
            log::info!("状态: {old:?} → {new_state:?}");
        }
    }

    fn set_conn(&mut self, status: &str, rssi: i32) {
        self.conn_status = status.to_string();
        // 断连时清零RSSI，避免UI显示过时信号值
        if status == "disconnected" || status == "ble_off" {
            self.conn_rssi = -100;
        } else if rssi > -90 {
            self.conn_rssi = rssi;
        }
        // 同步更新托盘状态
        let tray_state = match status {
            "connected" => TrayState::Connected,
            "fading" => TrayState::Fading,
            "disconnected" => TrayState::Disconnected,
            "ble_off" => TrayState::BleOff,
            "reconnecting" => TrayState::Reconnecting,
            _ => TrayState::Disconnected,
        };
        self.tray.update_status(tray_state, self.conn_rssi);
    }

    fn cancel_countdown(&mut self) {
        self.countdown_remaining = 0;
        self.countdown_started = false;
        self.countdown_expired = false;
        self.low_rssi_consecutive = 0;
        if self.app_state == AppState::Countdown {
            self.set_state(AppState::Monitoring);
        }
    }

    fn start_countdown(&mut self) {
        if matches!(
            self.app_state,
            AppState::Countdown | AppState::Locking | AppState::AutoPaused
        ) {
            return;
        }
        if self.countdown_started {
            return;
        }
        if !self.ever_connected {
            log::debug!("设备从未连接过，跳过锁屏倒计时");
            self.set_state(AppState::Monitoring);
            return;
        }
        if !self.config.auto_lock_enabled {
            log::debug!("自动锁屏已关闭，跳过锁屏倒计时");
            self.set_state(AppState::Monitoring);
            return;
        }
        log::info!("启动锁屏倒计时: {}秒", self.config.lock_delay_sec);
        self.countdown_started = true;
        self.countdown_expired = false;
        self.set_state(AppState::Countdown);
        self.countdown_remaining = self.config.lock_delay_sec;
        let rssi = self.current_rssi;
        self.set_conn("fading", rssi);
    }

    fn stop_auto_connect(&mut self) {
        self.auto_connecting = false;
        self.auto_connect_attempts = 0;
        self.lock_abandoned = false;
        self.retry_pending = false;
        self.retry_round_count = 0;
        self.connecting = false;
    }

    fn start_auto_connect(&mut self) {
        if self.config.phone_mac.is_empty() {
            return;
        }
        if self.current_connected {
            return;
        }
        self.auto_connecting = true;
        self.auto_connect_attempts = 0;
        self.set_conn("reconnecting", -100);
    }

    /// 断开所有已连接的蓝牙外设（非手机），需要时在锁屏前调用
    fn disconnect_bluetooth_peripherals(&self) {
        if !self.config.disconnect_peripherals {
            return;
        }
        // 使用 bluetoothctl 获取已连接设备列表，断开除手机外的设备
        let output = std::process::Command::new("bluetoothctl")
            .args(["devices", "Connected"])
            .output();
        if let Ok(out) = output {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let phone_mac = self.config.phone_mac.to_uppercase().replace('-', ":");
            for line in stdout.lines() {
                // 格式: "Device XX:XX:XX:XX:XX:XX DeviceName"
                if let Some(mac) = line.strip_prefix("Device ").and_then(|s| s.split_whitespace().next()) {
                    if mac.to_uppercase() != phone_mac {
                        log::info!("断开蓝牙外设: {mac}");
                        let _ = std::process::Command::new("bluetoothctl")
                            .args(["disconnect", mac])
                            .output();
                    }
                }
            }
        }
    }
}

// ── 主函数 ──────────────────────────────────────────────────────────────
fn main() {
    // 强制 GTK4 使用 Adwaita 深色主题（必须在 Application 创建前设置）
    std::env::set_var("GTK_THEME", "Adwaita:dark");
    // 当前环境无法访问 GPU/EGL 时，使用 Cairo 软件渲染避免 GTK4 在首次绘制阶段崩溃。
    std::env::set_var("GSK_RENDERER", "cairo");

    log_capture::init().expect("日志初始化失败");

    let mut cfg = Config::load();
    let args: Vec<String> = std::env::args().collect();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--mac" if i + 1 < args.len() => {
                cfg.phone_mac = args[i + 1].clone();
                i += 2;
            }
            "--delay" if i + 1 < args.len() => {
                if let Ok(v) = args[i + 1].parse() {
                    cfg.lock_delay_sec = v;
                }
                i += 2;
            }
            "--threshold" if i + 1 < args.len() => {
                if let Ok(v) = args[i + 1].parse() {
                    cfg.rssi_threshold = v;
                }
                i += 2;
            }
            "-v" | "--verbose" => {
                i += 1;
            }
            _ => {
                i += 1;
            }
        }
    }
    cfg.save();
    log::info!(
        "配置加载完成: phone_mac={}, auto_lock={}, auto_connect={}, threshold={}dBm, lock_delay={}s",
        cfg.phone_mac, cfg.auto_lock_enabled, cfg.auto_connect_device,
        cfg.rssi_threshold, cfg.lock_delay_sec
    );
    if cfg.language.is_empty() {
        cfg.language = detect_language();
        cfg.save();
    }
    set_lang(&cfg.language);

    let app = Application::builder()
        .application_id("io.github.owl.owlbtlock")
        .build();

    let core: SharedCore = new_core(cfg);

    app.connect_activate(move |application| {
        ui::apply_css();
        let phone_name = {
            let c = core.lock().unwrap();
            c.config.phone_name.clone()
        };
        let win = ApplicationWindow::builder()
            .application(application)
            .title(&format!(
                "{} - {}",
                tr("title"),
                if phone_name.is_empty() {
                    "--"
                } else {
                    &phone_name
                }
            ))
        .default_width({
            let c = core.lock().unwrap();
            c.config.window_width
        })
        .default_height({
            let c = core.lock().unwrap();
            c.config.window_height
        })
        .build();

        // 关闭窗口时隐藏到任务栏，而非退出程序
        win.connect_close_request(|win| {
            win.set_visible(false);
            log::info!("窗口已隐藏，程序继续在后台运行");
            gtk::glib::Propagation::Stop
        });

        // ── 构建 UI ──
        let main_box = GtkBox::new(gtk::Orientation::Vertical, 0);
        main_box.set_hexpand(true);
        // 顶栏
        let header = GtkBox::new(gtk::Orientation::Vertical, 0);
        header.set_margin_top(20);
        header.set_margin_start(20);
        header.set_margin_end(20);
        header.set_margin_bottom(5);
        let title_label = gtk::Label::new(Some(&tr("title")));
        title_label.add_css_class("title-label");
        title_label.set_halign(gtk::Align::Start);
        header.append(&title_label);
        let subtitle_label = gtk::Label::new(Some(&tr("subtitle")));
        subtitle_label.add_css_class("subtitle-label");
        subtitle_label.set_halign(gtk::Align::Start);
        header.append(&subtitle_label);
        main_box.append(&header);

        // ── 设备选择页 ──
        let pick_widgets = ui::build_pick_page();
        pick_widgets.container.set_vexpand(true);
        let page_container = GtkBox::new(gtk::Orientation::Vertical, 0);
        page_container.set_hexpand(true);
        page_container.set_visible(false);

        // ── 监控页（带回调）──
        // 使用 RefCell 延迟引用 mw（因为 mw 在 build_monitor_page 之后才创建）
        let mw_holder: Rc<RefCell<Option<Arc<Mutex<ui::MonitorWidgets>>>>> =
            Rc::new(RefCell::new(None));
        let mw_holder2: Rc<RefCell<Option<Arc<Mutex<ui::MonitorWidgets>>>>> = mw_holder.clone();
        let mw_holder3: Rc<RefCell<Option<Arc<Mutex<ui::MonitorWidgets>>>>> = mw_holder.clone();
        let mw: Arc<Mutex<ui::MonitorWidgets>> = Arc::new(Mutex::new(ui::build_monitor_page(
            {
                let c = core.clone();
                let w = win.clone();
                let mh = mw_holder2;
                move || {
                    if let Some(mw_ref) = mh.borrow().as_ref() {
                        on_toggle_monitor(&c, &w, mw_ref);
                    }
                }
            },
            {
                let c = core.clone();
                let dl = pick_widgets.device_list.clone();
                let win = win.clone();
                let pc = page_container.clone();
                let pk = pick_widgets.container.clone();
                let mh = mw_holder3;
                move || {
                    stop_monitoring(&c);
                    pc.set_visible(false);
                    pk.set_visible(true);
                    let devices = scanner::scan_connected_devices();
                    log::info!("刷新设备列表: {} 个设备", devices.len());
                    let c2 = c.clone();
                    let w2 = win.clone();
                    let pc2 = pc.clone();
                    let pk2 = pk.clone();
                    let mh2 = mh.clone();
                    let on_select: Rc<RefCell<Box<dyn Fn(&scanner::DeviceInfo)>>> =
                        Rc::new(RefCell::new(Box::new(move |dev| {
                            if let Some(mw_ref) = mh2.borrow().as_ref() {
                                select_device_from_pick(&c2, &w2, &pc2, &pk2, mw_ref, dev);
                            }
                        })));
                    let current_mac = c.lock().unwrap().config.phone_mac.clone();
                    ui::update_device_list(&dl, &devices, &current_mac, on_select);
                }
            },
        )));

        {
            let c = core.clone();
            let dl = pick_widgets.device_list.clone();
            let win = win.clone();
            let pc = page_container.clone();
            let pk = pick_widgets.container.clone();
            let mw_refresh = mw.clone();
            pick_widgets.btn_refresh.connect_clicked(move |_| {
                refresh_device_list_ui(&dl, &win, &c, &pc, &pk, &mw_refresh);
            });
        }
        // 返回按钮：返回监控页，重启监控
        {
            let c = core.clone();
            let pc = page_container.clone();
            let pk = pick_widgets.container.clone();
            let mw2 = mw.clone();
            pick_widgets.btn_back.connect_clicked(move |_| {
                pk.set_visible(false);
                pc.set_visible(true);
                // 重启监控（如果已配置设备）
                let has_device = !c.lock().unwrap().config.phone_mac.is_empty();
                if has_device {
                    start_monitoring(&c);
                    let should_connect = {
                        let mut c_guard = c.lock().unwrap();
                        let connected = c_guard.current_connected;
                        let rssi = c_guard.current_rssi;
                        if connected {
                            c_guard.ever_connected = true;
                            c_guard.set_state(AppState::Monitoring);
                            c_guard.set_conn("connected", rssi);
                            false
                        } else {
                            c_guard.set_state(AppState::Monitoring);
                            c_guard.start_auto_connect();
                            true
                        }
                    };
                    {
                        let c_guard = c.lock().unwrap();
                        update_ui(&c_guard, &mw2);
                    }
                    if should_connect {
                        start_connect_attempt(&c);
                    }
                }
            });
        }

        let mw_unlocked = mw.lock().unwrap();
        page_container.append(&mw_unlocked.container);
        drop(mw_unlocked);

        // 将 mw 放入 holder，使 toggle 回调可以访问
        *mw_holder.borrow_mut() = Some(mw.clone());

        main_box.append(&pick_widgets.container);
        main_box.append(&page_container);
        win.set_child(Some(&main_box));

        // 初始化显示 + 同步设置
        {
            let c = core.lock().unwrap();
            if c.config.phone_mac.is_empty() {
                drop(c);
                pick_widgets.container.set_visible(true);
                page_container.set_visible(false);
                let dl = pick_widgets.device_list.clone();
                let w2 = win.clone();
                let c2 = core.clone();
                let pc2 = page_container.clone();
                let pk2 = pick_widgets.container.clone();
                refresh_device_list_ui(&dl, &w2, &c2, &pc2, &pk2, &mw);
            } else {
                drop(c);
                pick_widgets.container.set_visible(false);
                page_container.set_visible(true);
                let mw_unlocked = mw.lock().unwrap();
                mw_unlocked.device_name.set_text(&core.lock().unwrap().config.phone_name);
                mw_unlocked.device_mac.set_text(&core.lock().unwrap().config.phone_mac);
                // 同步设置值到 UI
                let cfg = core.lock().unwrap().config.clone();
                ui::sync_settings_from_config(&mw_unlocked, &cfg);
                drop(mw_unlocked);
                let auto_start = cfg.auto_connect_device;
                drop(cfg);
                if auto_start {
                    start_monitoring(&core);
                    let should_connect = {
                        let mut c = core.lock().unwrap();
                        // 轮询线程已启动，直接使用缓存状态
                        let connected = c.current_connected;
                        let rssi = c.current_rssi;

                        if connected {
                            c.ever_connected = true;
                            c.set_state(AppState::Monitoring);
                            c.set_conn("connected", rssi);
                            false
                        } else {
                            c.set_state(AppState::Monitoring);
                            c.start_auto_connect();
                            true
                        }
                    };
                    let c = core.lock().unwrap();
                    update_ui(&c, &mw);
                    drop(c);
                    if should_connect {
                        start_connect_attempt(&core);
                    }
                }
            }
        }

        // ── 设置控件回调绑定 ──
        {
            let c = core.clone();
            let mw2 = mw.clone();
            let mw3 = mw.clone();
            // 锁屏延迟滑块
            {
                let c2 = c.clone();
                let slider = { mw2.lock().unwrap().slider_lock_delay.clone() };
                slider.connect_value_changed(move |s| {
                    let v = s.value() as i32;
                    c2.lock().unwrap().config.lock_delay_sec = v;
                    c2.lock().unwrap().config.save();
                });
            }
            // 信号阈值滑块
            {
                let c2 = c.clone();
                let slider = { mw2.lock().unwrap().slider_rssi_threshold.clone() };
                slider.connect_value_changed(move |s| {
                    let v = s.value() as i32;
                    let mut c = c2.lock().unwrap();
                    c.config.rssi_threshold = v;
                    c.config.save();
                    if let Some(tx) = &c.ctrl_tx {
                        let _ = tx.send(CtrlCmd::UpdateConfig {
                            threshold: v,
                            scan_interval: c.config.scan_interval_sec,
                        });
                    }
                });
            }
            // 轮询间隔滑块
            {
                let c2 = c.clone();
                let slider = { mw2.lock().unwrap().slider_scan_interval.clone() };
                slider.connect_value_changed(move |s| {
                    let v = s.value() as i32;
                    let mut c = c2.lock().unwrap();
                    c.config.scan_interval_sec = v;
                    c.config.save();
                    if let Some(tx) = &c.ctrl_tx {
                        let _ = tx.send(CtrlCmd::UpdateConfig {
                            threshold: c.config.rssi_threshold,
                            scan_interval: v,
                        });
                    }
                });
            }
            // 自动锁屏开关
            {
                let c2 = c.clone();
                let switch = { mw2.lock().unwrap().switch_auto_lock.clone() };
                switch.connect_active_notify(move |s| {
                    c2.lock().unwrap().config.auto_lock_enabled = s.is_active();
                    c2.lock().unwrap().config.save();
                });
            }
            // 自动解锁开关
            {
                let c2 = c.clone();
                let switch = { mw2.lock().unwrap().switch_auto_unlock.clone() };
                switch.connect_active_notify(move |s| {
                    c2.lock().unwrap().config.auto_unlock_on_reconnect = s.is_active();
                    c2.lock().unwrap().config.save();
                });
            }
            // 自动重连开关
            {
                let c2 = c.clone();
                let switch = { mw2.lock().unwrap().switch_auto_connect.clone() };
                switch.connect_active_notify(move |s| {
                    c2.lock().unwrap().config.auto_connect_device = s.is_active();
                    c2.lock().unwrap().config.save();
                });
            }
            // 锁屏后恢复监控开关
            {
                let c2 = c.clone();
                let switch = { mw2.lock().unwrap().switch_auto_resume.clone() };
                switch.connect_active_notify(move |s| {
                    c2.lock().unwrap().config.auto_resume_on_reconnect = s.is_active();
                    c2.lock().unwrap().config.save();
                });
            }
            // 断开蓝牙外设开关
            {
                let c2 = c.clone();
                let switch = { mw2.lock().unwrap().switch_disconnect_peripherals.clone() };
                switch.connect_active_notify(move |s| {
                    c2.lock().unwrap().config.disconnect_peripherals = s.is_active();
                    c2.lock().unwrap().config.save();
                });
            }
            // 开机自启开关
            {
                let c2 = c.clone();
                let switch = { mw2.lock().unwrap().switch_auto_start.clone() };
                switch.connect_active_notify(move |s| {
                    let enabled = s.is_active();
                    c2.lock().unwrap().config.auto_start_on_boot = enabled;
                    c2.lock().unwrap().config.save();
                    if enabled {
                        crate::autostart::enable_autostart();
                    } else {
                        crate::autostart::disable_autostart();
                    }
                });
            }
            // 语言下拉框
            {
                let c2 = c.clone();
                let mw_ref = mw2.clone();
                let combo = { mw2.lock().unwrap().combo_language.clone() };
                combo.connect_selected_notify(move |ddl| {
                    let lang = match ddl.selected() {
                        1 => "en-US",
                        _ => "zh-CN",
                    };
                    c2.lock().unwrap().config.language = lang.to_string();
                    c2.lock().unwrap().config.save();
                    crate::i18n::set_lang(lang);
                    // 刷新所有设置标签
                    let mw_ui = mw_ref.lock().unwrap();
                    ui::refresh_labels(&mw_ui);
                    drop(mw_ui);
                    log::info!("语言已切换: {lang}");
                });
            }
            // 恢复默认按钮
            {
                let c2 = c.clone();
                let mw_ref = mw3.clone();
                let btn = { mw3.lock().unwrap().btn_restore_defaults.clone() };
                btn.connect_clicked(move |_| {
                    let default_cfg = Config::default();
                    {
                        let mut c = c2.lock().unwrap();
                        c.config.lock_delay_sec = default_cfg.lock_delay_sec;
                        c.config.rssi_threshold = default_cfg.rssi_threshold;
                        c.config.scan_interval_sec = default_cfg.scan_interval_sec;
                        c.config.auto_lock_enabled = default_cfg.auto_lock_enabled;
                        c.config.auto_unlock_on_reconnect = default_cfg.auto_unlock_on_reconnect;
                        c.config.auto_connect_device = default_cfg.auto_connect_device;
                        c.config.auto_resume_on_reconnect = default_cfg.auto_resume_on_reconnect;
                        c.config.disconnect_peripherals = default_cfg.disconnect_peripherals;
                        c.config.auto_start_on_boot = default_cfg.auto_start_on_boot;
                        c.config.language = default_cfg.language;
                        c.config.save();
                    }
                    // 禁用自启
                    crate::autostart::disable_autostart();
                    let cfg = c2.lock().unwrap().config.clone();
                    let mw_ui = mw_ref.lock().unwrap();
                    ui::sync_settings_from_config(&mw_ui, &cfg);
                    log::info!("设置已恢复默认值");
                });
            }
            // 诊断按钮（日志 + RSSI 曲线）
            {
                let btn = { mw3.lock().unwrap().btn_log.clone() };
                btn.connect_clicked(move |_| {
                    ui::show_log_dialog();
                });
            }
        }

        // ── 定时器 ──
        // 轮询线程消息接收（每100ms检查 channel，足够覆盖 3s 轮询周期，唤醒次数减半）
        {
            let core = core.clone();
            let mw2 = mw.clone();
            glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
                let msgs: Vec<PollMsg> = {
                    let c = core.lock().unwrap();
                    if let Some(rx) = &c.poll_rx {
                        rx.try_iter().collect()
                    } else {
                        return glib::ControlFlow::Continue;
                    }
                };
                for msg in msgs {
                    match msg {
                        PollMsg::Status { connected, rssi, rssi_supported, fast_poll: _, countdown: _ } => {
                            let mut c = core.lock().unwrap();
                            c.current_connected = connected;
                            c.current_rssi = rssi;
                            c.current_rssi_supported = rssi_supported;
                            if connected {
                                c.ever_connected = true;
                                c.set_conn("connected", rssi);
                            } else if c.app_state == AppState::Monitoring {
                                c.set_conn("disconnected", -100);
                            }
                            update_ui(&c, &mw2);
                        }
                        PollMsg::Event(event) => {
                            handle_bt_event(&core, event, &mw2);
                        }
                        PollMsg::ConnectResult { success, attempt: _ } => {
                            if success {
                                on_reconnect_success(&core);
                            } else {
                                after_connect_failed(&core);
                            }
                            let c = core.lock().unwrap();
                            update_ui(&c, &mw2);
                        }
                    }
                }
                glib::ControlFlow::Continue
            });
        }
        // 倒计时（每1秒）
        {
            let c = core.clone();
            let mw2 = mw.clone();
            glib::timeout_add_seconds_local(1, move || {
                countdown_tick(&c, &mw2);
                glib::ControlFlow::Continue
            });
        }
        // 启动托盘，并处理托盘动作（显示窗口/退出）
        {
            let mut c = core.lock().unwrap();
            c.tray.start();
        }
        // 获取托盘动作接收器
        let action_rx = core.lock().unwrap().tray.take_action_receiver();
        if let Some(rx) = action_rx {
            let win_ref = win.clone();
            let app_ref = application.clone();
            let c = core.clone();
            gtk::glib::timeout_add_local(std::time::Duration::from_millis(200), move || {
                match rx.try_recv() {
                    Ok(TrayAction::Show) => {
                        log::info!("托盘动作: 显示窗口");
                        win_ref.set_visible(true);
                        win_ref.present();
                    }
                    Ok(TrayAction::Quit) => {
                        log::info!("托盘动作: 退出程序");
                        c.lock().unwrap().tray.stop();
                        app_ref.quit();
                        return gtk::glib::ControlFlow::Break;
                    }
                    Err(mpsc::TryRecvError::Disconnected) => {
                        return gtk::glib::ControlFlow::Break;
                    }
                    _ => {}
                }
                gtk::glib::ControlFlow::Continue
            });
        }

        win.present();
    });

    // GTK4 的 Application 会自动处理 SIGTERM 信号
    app.run();
}

// ── 页面切换 ──────────────────────────────────────────────────────────────

// ── 设备选择 ──────────────────────────────────────────────────────────────

// ── 监控控制 ──────────────────────────────────────────────────────────────

fn start_monitoring(core: &SharedCore) {
    let mut c = core.lock().unwrap();
    if c.config.phone_mac.is_empty() {
        return;
    }
    // 若已经在监控，先停掉旧的轮询线程，避免重复轮询 / 双线程
    if c.ctrl_tx.is_some() {
        if let Some(tx) = c.ctrl_tx.take() {
            let _ = tx.send(CtrlCmd::Stop);
        }
        c.poll_rx.take();
        log::info!("检测到已有监控线程，已发送停止信号，准备重启");
    }
    log::info!(
        "启动监控: device={}, auto_lock={}, auto_connect={}",
        c.config.phone_mac,
        c.config.auto_lock_enabled,
        c.config.auto_connect_device
    );
    let mut bt = BluetoothMonitor::new(&c.config.phone_mac, c.config.rssi_threshold);
    if bt.setup() {
        log::info!("蓝牙监控初始化成功");
    } else {
        log::warn!("蓝牙监控初始化失败，等待适配器就绪");
    }
    let connected = bt.connected;
    let rssi = bt.current_rssi;

    // 创建轮询线程通道
    let (poll_tx, poll_rx) = mpsc::channel::<PollMsg>();
    let (ctrl_tx, ctrl_rx) = mpsc::channel::<CtrlCmd>();

    // 让蓝牙监控器直接把适配器开/关等事件送入与轮询结果相同的通道，
    // 否则 AdapterPoweredOff/On 事件永远发不出来（此前 set_event_sender 从未被调用）。
    bt.set_event_sender(poll_tx.clone());

    // 创建 SmartPoller 并启动
    let mut poller = SmartPoller::new(c.config.rssi_threshold, c.config.scan_interval_sec);
    poller.start();

    // 存储通道到 AppCore
    c.poll_rx = Some(poll_rx);
    c.ctrl_tx = Some(ctrl_tx);

    // 启动轮询线程
    thread::spawn(move || {
        bt_poll_loop(bt, poller, poll_tx, ctrl_rx);
    });

    c.set_state(AppState::Monitoring);
    if connected {
        c.ever_connected = true;
        c.set_conn("connected", rssi);
    } else {
        c.set_conn("disconnected", -100);
    }
}

fn stop_monitoring(core: &SharedCore) {
    log::info!("停止监控");
    let mut c = core.lock().unwrap();
    c.ever_connected = false;
    c.cancel_countdown();
    c.stop_auto_connect();
    // 通知轮询线程停止
    if let Some(tx) = c.ctrl_tx.take() {
        let _ = tx.send(CtrlCmd::Stop);
    }
    c.poll_rx.take();
    c.set_state(AppState::Idle);
    c.set_conn("disconnected", -100);
}

fn on_toggle_monitor(
    core: &SharedCore,
    _win: &ApplicationWindow,
    mw: &Arc<Mutex<ui::MonitorWidgets>>,
) {
    let state = core.lock().unwrap().app_state;
    match state {
        AppState::Idle => start_monitoring(core),
        AppState::AutoPaused => {
            let mut c = core.lock().unwrap();
            c.ever_connected = true;
            c.lock_abandoned = false;
            c.set_state(AppState::Monitoring);
        }
        _ => stop_monitoring(core),
    }
    // 立即更新UI，给用户即时反馈
    let c = core.lock().unwrap();
    update_ui(&c, mw);
}

fn refresh_device_list_ui(
    device_list: &gtk::Box,
    win: &ApplicationWindow,
    core: &SharedCore,
    page_container: &GtkBox,
    pick_container: &GtkBox,
    mw: &Arc<Mutex<ui::MonitorWidgets>>,
) {
    let devices = scanner::scan_connected_devices();
    log::info!("刷新设备列表: {} 个设备", devices.len());
    let c = core.clone();
    let pc = page_container.clone();
    let pk = pick_container.clone();
    let w = win.clone();
    let mw_owned = mw.clone();
    let on_select: Rc<RefCell<Box<dyn Fn(&scanner::DeviceInfo)>>> =
        Rc::new(RefCell::new(Box::new(move |dev| {
            select_device_from_pick(&c, &w, &pc, &pk, &mw_owned, dev);
        })));
    let current_mac = core.lock().unwrap().config.phone_mac.clone();
    ui::update_device_list(device_list, &devices, &current_mac, on_select);
}

fn select_device_from_pick(
    core: &SharedCore,
    win: &ApplicationWindow,
    page_container: &GtkBox,
    pick_container: &GtkBox,
    mw: &Arc<Mutex<ui::MonitorWidgets>>,
    dev: &scanner::DeviceInfo,
) {
    log::info!("用户选择监控设备: {} ({})", dev.name, dev.mac);
    {
        let mut c = core.lock().unwrap();
        c.config.phone_mac = dev.mac.clone();
        c.config.phone_name = dev.name.clone();
        // 用户的选择是唯一事实来源：立刻持久化，且只写这一份配置。
        c.config.save();
        // 换设备后不能沿用上一台的运行状态，否则「从未连接过」的守卫会失效，
        // 新设备第一次断连就可能被当成「远离」而直接锁屏。
        c.ever_connected = false;
        c.current_connected = false;
        c.current_rssi = -100;
        c.current_rssi_supported = false;
        c.low_rssi_consecutive = 0;
        c.cancel_countdown();
        c.stop_auto_connect();
    }
    pick_container.set_visible(false);
    page_container.set_visible(true);
    win.set_title(Some(&format!("{} - {}", tr("title"), &dev.name)));
    // 直接通过 mw 引用刷新设备名/MAC 标签。
    // 不能用 update_monitor_device_info 的"文本为 -- 才更新"逻辑：
    // 更换设备时旧标签已是真实数据，会被该判断跳过，导致界面仍显示旧设备。
    {
        let cfg = core.lock().unwrap().config.clone();
        let mw_ui = mw.lock().unwrap();
        mw_ui.device_name.set_text(&dev.name);
        mw_ui.device_mac.set_text(&dev.mac);
        // 将已保存的配置同步到 UI 开关/滑块，避免切换设备后开关回退为默认值
        ui::sync_settings_from_config(&mw_ui, &cfg);
    }
    start_monitoring(core);
}

// ── 蓝牙事件处理 ──────────────────────────────────────────────────────────

fn handle_bt_event(core: &SharedCore, event: BtEvent, mw: &Arc<Mutex<ui::MonitorWidgets>>) {
    let mut c = core.lock().unwrap();
    log::info!("[事件] 收到 {:?}, paused={}, state={:?}", event, c.paused, c.app_state);
    if c.paused {
        return;
    }
    match event {
        BtEvent::Connected => {
            c.ever_connected = true;
            c.low_rssi_consecutive = 0;
            if c.app_state == AppState::Monitoring {
                return;
            }
            let rssi = c.current_rssi;
            log::info!("设备已连接: rssi={rssi}dBm, 状态={:?}", c.app_state);
            c.stop_auto_connect();
            c.cancel_countdown();
            c.set_state(AppState::Monitoring);
            c.set_conn("connected", rssi);
            if c.config.auto_unlock_on_reconnect {
                locker::unlock_session();
            }
            // 确保UI更新
            update_ui(&c, &mw);
        }
        BtEvent::Disconnected => {
            if c.paused || c.app_state == AppState::Locking {
                return;
            }
            if c.app_state == AppState::AutoPaused {
                if c.config.auto_connect_device && !c.auto_connecting && !c.retry_pending {
                    c.start_auto_connect();
                }
                return;
            }
            if !matches!(c.app_state, AppState::Monitoring | AppState::Countdown) {
                return;
            }
            if c.retry_pending {
                return;
            }
            // 断开时始终更新连接状态
            c.set_conn("disconnected", -100);
            // 启动锁屏倒计时（倒计时中每秒检测用户活动，活跃则取消）
            c.start_countdown();
            // 先在锁内记录开关与状态，避免后续释放锁后重入死锁
            let auto_cfg = c.config.auto_connect_device;
            let already_auto = c.auto_connecting;
            if auto_cfg && !already_auto {
                c.start_auto_connect();
            }
            // 确保UI更新（此时仍持有锁）
            update_ui(&c, &mw);
            // 释放锁后再触发后续动作，避免对非重入 Mutex 重入死锁
            drop(c);
            if auto_cfg && !already_auto {
                // 真正发起蓝牙重连（之前只置了标志却从不执行，这里补齐）
                start_connect_attempt(core);
            } else if !auto_cfg {
                // 未开启自动重连：处理「用户活跃期间跳过锁屏」逻辑
                judge_after_unreachable(core);
            }
        }
        BtEvent::RssiChanged(rssi) => {
            if c.paused || !c.current_connected {
                return;
            }
            if !c.current_rssi_supported {
                return;
            }
            if matches!(
                c.app_state,
                AppState::Locking | AppState::Idle | AppState::AutoPaused
            ) {
                return;
            }
            let status = c.conn_status.clone();
            c.set_conn(&status, rssi);
            if c.app_state == AppState::Countdown {
                if rssi >= c.config.rssi_threshold {
                    log::info!(
                        "信号恢复: {rssi}dBm >= 阈值{}dBm，取消锁屏",
                        c.config.rssi_threshold
                    );
                    c.low_rssi_consecutive = 0;
                    c.cancel_countdown();
                    c.set_state(AppState::Monitoring);
                }
                return;
            }
            if rssi < c.config.rssi_threshold {
                c.low_rssi_consecutive += 1;
                log::info!(
                    "信号低于阈值: {rssi}dBm < {}dBm ({}/3)",
                    c.config.rssi_threshold,
                    c.low_rssi_consecutive
                );
                if c.low_rssi_consecutive >= 3 {
                    c.low_rssi_consecutive = 0;
                    c.start_countdown();
                }
            } else {
                c.low_rssi_consecutive = 0;
            }
        }
        BtEvent::AdapterPoweredOff => {
            log::info!("蓝牙适配器已关闭");
            c.cancel_countdown();
            c.set_state(AppState::BleTurnedOff);
            c.set_conn("ble_off", -100);
            // 确保UI更新
            update_ui(&c, &mw);
        }
        BtEvent::AdapterPoweredOn => {
            log::info!("蓝牙适配器已开启");
            let mut need_connect = false;
            if c.app_state == AppState::BleTurnedOff {
                c.set_state(AppState::Monitoring);
                if c.current_connected {
                    let rssi = c.current_rssi;
                    c.set_conn("connected", rssi);
                } else if c.config.auto_connect_device && !c.config.phone_mac.is_empty() {
                    c.set_conn("reconnecting", -100);
                    if !c.auto_connecting {
                        c.start_auto_connect();
                        need_connect = true;
                    }
                } else {
                    c.set_conn("disconnected", -100);
                }
            }
            // 确保UI更新（此时仍持有锁）
            update_ui(&c, &mw);
            // 释放锁后再发起重连，避免重入死锁
            drop(c);
            if need_connect {
                start_connect_attempt(core);
            }
        }
    }
}

// ── 倒计时 ────────────────────────────────────────────────────────────────

fn countdown_tick(core: &SharedCore, _mw: &Arc<Mutex<ui::MonitorWidgets>>) {
    let mut c = core.lock().unwrap();
    if c.app_state == AppState::Countdown && c.countdown_remaining > 0 {
        let idle_ms = locker::get_idle_ms();
        log::debug!(
            "[倒计时] remaining={}s, idle={:?}ms",
            c.countdown_remaining, idle_ms
        );
        // Some(0) 表示用户刚刚有过操作（鼠标/键盘），取消锁屏
        // None 表示无法检测（视为用户未活动，允许锁屏）
        if idle_ms == Some(0) {
            log::info!("检测到用户活动（idle=0ms），取消锁屏倒计时");
            c.cancel_countdown();
            c.lock_abandoned = true;
            return;
        }
        c.countdown_remaining -= 1;
        if c.countdown_remaining <= 0 {
            c.countdown_expired = true;
            drop(c);
            do_lock_screen(core);
            return;
        }
    } else if c.lock_abandoned && c.app_state == AppState::Monitoring {
        // 此前因用户活跃而跳过了锁屏：一旦用户恢复空闲，立即重新启动倒计时，
        // 否则在关闭自动重连时锁屏会永久失效。
        let idle_ms = locker::get_idle_ms();
        if idle_ms != Some(0) {
            log::info!("用户已停止活动，重新启动锁屏倒计时");
            c.lock_abandoned = false;
            c.start_countdown();
        }
    }
}

fn do_lock_screen(core: &SharedCore) {
    let mut c = core.lock().unwrap();
    if matches!(c.app_state, AppState::Locking | AppState::AutoPaused) {
        return;
    }
    let idle_ms = locker::get_idle_ms();
    if idle_ms == Some(0) {
        log::info!("锁屏前检测到用户活动（idle=0ms），取消锁屏");
        c.set_state(AppState::Monitoring);
        c.countdown_started = false;
        c.lock_abandoned = true;
        return;
    }
    log::info!("执行锁屏");
    c.disconnect_bluetooth_peripherals();
    c.set_state(AppState::Locking);
    c.countdown_started = false;
    let locked = locker::lock_screen();
    if locked {
        if c.config.auto_resume_on_reconnect {
            c.set_state(AppState::AutoPaused);
        } else {
            drop(c);
            stop_monitoring(core);
            return;
        }
    } else {
        c.set_state(AppState::Monitoring);
    }
}

// ── 自动连接 ──────────────────────────────────────────────────────────────

fn start_connect_attempt(core: &SharedCore) {
    let mut c = core.lock().unwrap();
    if c.connecting {
        return;
    }
    if c.auto_connect_attempts >= MAX_CONNECT_ATTEMPTS {
        return;
    }
    c.connecting = true;
    c.auto_connect_attempts += 1;
    // 通过 CtrlCmd 通知轮询线程执行连接
    if let Some(tx) = &c.ctrl_tx {
        let _ = tx.send(CtrlCmd::Connect);
    }
}

fn kill_child(child: &mut Option<std::process::Child>) {
    if let Some(mut proc) = child.take() {
        let _ = proc.kill();
    }
}

fn kill_procs(le: &mut Option<std::process::Child>, br: &mut Option<std::process::Child>) {
    for p in [le, br].iter_mut() {
        if let Some(mut proc) = p.take() {
            let _ = proc.kill();
        }
    }
}

fn on_reconnect_success(core: &SharedCore) {
    let mut c = core.lock().unwrap();
    log::info!("重连成功: 状态={:?}", c.app_state);
    c.ever_connected = true;
    c.low_rssi_consecutive = 0;
    c.lock_abandoned = false;
    c.retry_round_count = 0;
    c.retry_pending = false;
    if c.app_state == AppState::AutoPaused {
        c.set_state(AppState::Monitoring);
        let rssi = c.current_rssi;
        c.set_conn("connected", rssi);
        if c.config.auto_unlock_on_reconnect {
            locker::unlock_session();
        }
        return;
    }
    c.auto_connecting = false;
    c.auto_connect_attempts = 0;
    c.cancel_countdown();
    c.set_state(AppState::Monitoring);
    let rssi = c.current_rssi;
    c.set_conn("connected", rssi);
    if c.config.auto_unlock_on_reconnect {
        locker::unlock_session();
    }
}

fn after_connect_failed(core: &SharedCore) {
    let attempts = core.lock().unwrap().auto_connect_attempts;
    if attempts < MAX_CONNECT_ATTEMPTS {
        let core_clone = core.clone();
        glib::timeout_add_seconds_local(CONNECT_ATTEMPT_INTERVAL_SEC, move || {
            start_connect_attempt(&core_clone);
            glib::ControlFlow::Break
        });
        return;
    }
    log::info!("连续 {MAX_CONNECT_ATTEMPTS} 次连接失败，判定手机不在周围");
    judge_after_unreachable(core);
}

fn judge_after_unreachable(core: &SharedCore) {
    // 注意：本函数自行加锁，调用方必须先释放 AppCore 锁，避免对非重入 Mutex 重入导致死锁。
    let mut c = core.lock().unwrap();
    if c.app_state == AppState::AutoPaused {
        drop(c);
        schedule_retry_round(core);
        return;
    }
    if c.lock_abandoned {
        // 用户活跃期间跳过锁屏，当用户停止活动后（idle>0或无法检测），清除lock_abandoned允许重新锁屏
        let idle_ms = locker::get_idle_ms();
        if idle_ms != Some(0) {
            log::info!("用户已停止活动（idle={:?}ms），清除lock_abandoned标志，准备重新锁屏", idle_ms);
            c.lock_abandoned = false;
            c.start_countdown();
        } else {
            drop(c);
            schedule_retry_round(core);
            return;
        }
    } else {
        let idle_ms = locker::get_idle_ms();
        if idle_ms != Some(0) {
            c.start_countdown();
        } else {
            c.lock_abandoned = true;
            log::info!("电脑处于活跃状态，本轮不再锁屏，只持续尝试重连");
        }
    }
    drop(c);
    schedule_retry_round(core);
}

fn schedule_retry_round(core: &SharedCore) {
    let mut c = core.lock().unwrap();
    if !c.config.auto_connect_device || c.retry_pending {
        return;
    }
    c.auto_connecting = true;
    c.retry_pending = true;
    let delay = std::cmp::min(
        RETRY_BASE_INTERVAL_SEC * 2u64.pow(c.retry_round_count as u32),
        RETRY_MAX_INTERVAL_SEC,
    ) as u32;
    let round = c.retry_round_count;
    log::info!("将在 {delay}秒 后进行第 {round} 轮重连");
    drop(c);
    let core_clone = core.clone();
    glib::timeout_add_seconds_local(delay, move || {
        retry_round(&core_clone);
        glib::ControlFlow::Break
    });
}

fn retry_round(core: &SharedCore) {
    let mut c = core.lock().unwrap();
    c.retry_pending = false;
    if matches!(
        c.app_state,
        AppState::Locking | AppState::Idle | AppState::BleTurnedOff
    ) || c.paused
    {
        c.auto_connecting = false;
        c.retry_round_count = 0;
        return;
    }
    if c.current_connected {
        c.auto_connecting = false;
        c.retry_round_count = 0;
        return;
    }
    if c.connecting {
        drop(c);
        schedule_retry_round(core);
        return;
    }
    c.retry_round_count += 1;
    c.auto_connect_attempts = 0;
    drop(c);
    start_connect_attempt(core);
}

// ── UI 刷新 ────────────────────────────────────────────────────────────────

fn update_ui(c: &AppCore, mw: &Arc<Mutex<ui::MonitorWidgets>>) {
    let conn = &c.conn_status;
    let rssi = c.conn_rssi;
    let mw = mw.lock().unwrap();
    ui::set_status_dot_color(&mw.status_dot, conn);
    ui::update_conn_status(&mw.conn_status, conn);
    if rssi > -90 {
        mw.rssi_text.set_text(&format!("{rssi} dBm"));
        ui::update_signal_bar(&mw.signal_bar, rssi);
    } else {
        mw.rssi_text.set_text("- dBm");
        mw.signal_bar.set_fraction(0.0);
    }
    let state_str = match c.app_state {
        AppState::Idle => "idle",
        AppState::AutoPaused => "auto_paused",
        _ => "active",
    };
    ui::update_toggle_button(&mw.btn_toggle, state_str);
}

// ── 轮询线程 ──────────────────────────────────────────────────────────────

/// 蓝牙轮询线程主循环
/// 在独立线程中执行所有阻塞的蓝牙操作，通过 channel 将结果发送到主线程
fn bt_poll_loop(
    mut bt: BluetoothMonitor,
    mut poller: SmartPoller,
    poll_tx: mpsc::Sender<PollMsg>,
    ctrl_rx: mpsc::Receiver<CtrlCmd>,
) {
    log::info!("[轮询线程] 启动");

    // 跟踪连接进程
    let mut connect_le: Option<std::process::Child> = None;
    let mut connect_br: Option<std::process::Child> = None;
    let mut connect_start: Option<std::time::Instant> = None;
    // 跟踪上一次连接状态，用于检测变化
    let mut was_connected = bt.connected;

    loop {
        // 检查控制命令
        while let Ok(cmd) = ctrl_rx.try_recv() {
            match cmd {
                CtrlCmd::UpdateConfig { threshold, scan_interval } => {
                    poller.set_threshold(threshold);
                    poller.set_scan_interval(scan_interval);
                    log::debug!("[轮询线程] 配置更新: threshold={threshold}, interval={scan_interval}s");
                }
                CtrlCmd::Connect => {
                    log::info!("[轮询线程] 收到连接命令");
                    let (le, br) = bt.start_connect_process(CONNECT_TIMEOUT_SEC);
                    connect_le = le;
                    connect_br = br;
                    connect_start = Some(std::time::Instant::now());
                }
                CtrlCmd::Stop => {
                    log::info!("[轮询线程] 收到停止命令");
                    // 清理连接进程
                    kill_child(&mut connect_le);
                    kill_child(&mut connect_br);
                    return;
                }
            }
        }

        // 检查连接进程状态
        if connect_start.is_some() {
            let elapsed = connect_start.unwrap().elapsed();
            let timeout = std::time::Duration::from_secs(CONNECT_TIMEOUT_SEC + 10);
            let le_done = connect_le.as_mut().map(|p| p.try_wait().ok().flatten().is_some()).unwrap_or(true);
            let br_done = connect_br.as_mut().map(|p| p.try_wait().ok().flatten().is_some()).unwrap_or(true);

            if elapsed > timeout {
                log::info!("[轮询线程] 连接超时");
                kill_child(&mut connect_le);
                kill_child(&mut connect_br);
                connect_start = None;
                let _ = poll_tx.send(PollMsg::ConnectResult { success: false, attempt: 0 });
            } else if le_done && br_done {
                // 进程结束，检查是否连上
                let connected = bt.check_connected_now();
                bt.connected = connected;
                kill_child(&mut connect_le);
                kill_child(&mut connect_br);
                connect_start = None;
                if connected {
                    log::info!("[轮询线程] 连接成功");
                    let _ = poll_tx.send(PollMsg::ConnectResult { success: true, attempt: 0 });
                    let _ = poll_tx.send(PollMsg::Event(BtEvent::Connected));
                } else {
                    log::info!("[轮询线程] 连接失败");
                    let _ = poll_tx.send(PollMsg::ConnectResult { success: false, attempt: 0 });
                }
            }
            // 连接过程中仍然继续轮询状态
        }

        // 执行蓝牙状态轮询（阻塞操作，在线程里无所谓）
        bt.poll_status();

        let connected = bt.connected;
        let rssi = bt.filtered_rssi;
        let rssi_supported = bt.rssi_supported;

        // 记录 RSSI 到历史（无条件，诊断页面需要完整时间序列）
        crate::log_capture::push_rssi(rssi);

        // 智能轮询计算
        let (next_ms, trigger_disconnect) = poller.tick(connected, rssi, rssi_supported);
        let fast = poller.is_fast_polling();

        // 蓝牙去抖动进行中时，强制加速轮询（0.5s）以加快确认速度
        let sleep_ms = if bt.is_state_changing() {
            log::debug!("[轮询] 去抖动进行中，强制加速 500ms");
            500
        } else {
            next_ms
        };

        log::debug!(
            "[轮询] connected={}, rssi={}dBm, supported={}, fast={}, sleep_ms={}",
            connected, rssi, rssi_supported, fast, sleep_ms
        );

        // 发送状态更新到主线程
        let _ = poll_tx.send(PollMsg::Status {
            connected,
            rssi,
            rssi_supported,
            fast_poll: fast,
            countdown: (next_ms / 1000).max(1) as i32,
        });

        // 检测连接状态变化，发送事件
        if !connected && was_connected {
            log::info!("[轮询线程] 检测到断开: trigger_disconnect={}, was_connected={}, connected={}", trigger_disconnect, was_connected, connected);
            let _ = poll_tx.send(PollMsg::Event(BtEvent::Disconnected));
        } else if connected && !was_connected {
            log::info!("[轮询线程] 检测到连接");
            let _ = poll_tx.send(PollMsg::Event(BtEvent::Connected));
        } else if trigger_disconnect && connected {
            // RSSI 持续低于阈值，触发远离事件
            log::info!("[轮询线程] RSSI 低于阈值，触发远离");
            let _ = poll_tx.send(PollMsg::Event(BtEvent::Disconnected));
        }
        was_connected = connected;

        // 检测 RSSI 变化（仅连接且支持 RSSI 时）
        if connected && rssi_supported && rssi > -90 {
            let _ = poll_tx.send(PollMsg::Event(BtEvent::RssiChanged(rssi)));
        }

        // 根据轮询间隔 sleep
        thread::sleep(Duration::from_millis(sleep_ms.max(100) as u64));
    }
}
