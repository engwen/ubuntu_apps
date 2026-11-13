/// StatusNotifierItem D-Bus 托盘实现
/// 兼容 KDE Plasma / XFCE / Cinnamon / MATE / Unity / Deepin / Ubuntu(GNOME+AppIndicator)
use std::sync::{mpsc, Arc, Mutex};
use std::process::Command;
use zbus::connection::Builder;
use zbus::interface;
use zbus::zvariant::{Array, Dict, OwnedValue, Signature, Str, StructureBuilder, Value};

use super::{TrayAction, TrayBackend, TrayState};

/// 检查 StatusNotifierWatcher 是否存在
pub fn watcher_exists() -> bool {
    let output = Command::new("gdbus")
        .args([
            "call", "--session",
            "--dest", "org.kde.StatusNotifierWatcher",
            "--object-path", "/StatusNotifierWatcher",
            "--method", "org.freedesktop.DBus.Properties.Get",
            "org.kde.StatusNotifierWatcher", "ProtocolVersion",
        ])
        .output();
    match output {
        Ok(out) => out.status.success(),
        Err(_) => false,
    }
}

/// D-Bus 上共享的托盘状态
struct SnIState {
    status: TrayState,
    rssi: i32,
}

/// 全局动作通道（DBusMenu 无法直接持有 SnIItem 引用）
static TRAY_ACTION_TX: Mutex<Option<mpsc::Sender<TrayAction>>> = Mutex::new(None);

/// StatusNotifierItem D-Bus 接口实现
#[derive(Clone)]
struct SnIItem {
    state: Arc<Mutex<SnIState>>,
    menu_path: String,
}

#[interface(name = "org.kde.StatusNotifierItem")]
impl SnIItem {
    #[zbus(property)]
    fn category(&self) -> &str {
        "ApplicationStatus"
    }

    #[zbus(property)]
    fn id(&self) -> &str {
        "owlbtlock"
    }

    #[zbus(property)]
    fn title(&self) -> &str {
        "OwlBTLock"
    }

    #[zbus(property)]
    fn status(&self) -> &str {
        let state = self.state.lock().unwrap();
        match state.status {
            TrayState::Connected => "Active",
            TrayState::Fading => "NeedsAttention",
            TrayState::Disconnected => "NeedsAttention",
            TrayState::Paused => "Passive",
            TrayState::BleOff => "NeedsAttention",
            TrayState::Reconnecting => "NeedsAttention",
        }
    }

    #[zbus(property)]
    fn icon_name(&self) -> &str {
        "preferences-system-privacy"
    }

    #[zbus(property)]
    fn icon_pixmap(&self) -> Vec<(i32, i32, Vec<u8>)> {
        generate_icon_pixmap()
    }

    #[zbus(property)]
    fn tool_tip(&self) -> (String, Vec<(i32, i32, Vec<u8>)>, String, String) {
        let state = self.state.lock().unwrap();
        let (summary, body) = match state.status {
            TrayState::Connected => (
                "已连接".to_string(),
                format!("信号: {} dBm", state.rssi),
            ),
            TrayState::Fading => (
                "信号减弱".to_string(),
                format!("信号: {} dBm", state.rssi),
            ),
            TrayState::Disconnected => (
                "未连接".to_string(),
                "蓝牙设备已断开".to_string(),
            ),
            TrayState::Paused => (
                "监控已暂停".to_string(),
                String::new(),
            ),
            TrayState::BleOff => (
                "蓝牙已关闭".to_string(),
                String::new(),
            ),
            TrayState::Reconnecting => (
                "正在重连".to_string(),
                String::new(),
            ),
        };
        (String::new(), Vec::new(), summary, body)
    }

    #[zbus(property)]
    fn menu(&self) -> zbus::zvariant::OwnedObjectPath {
        zbus::zvariant::ObjectPath::from_str_unchecked(&self.menu_path).into()
    }

    /// 左击托盘图标 → 显示窗口
    async fn activate(&self, _x: i32, _y: i32) {
        log::info!("托盘图标左击 → 显示窗口");
        if let Some(tx) = TRAY_ACTION_TX.lock().unwrap().as_ref() {
            let _ = tx.send(TrayAction::Show);
        }
    }

    /// 中键点击
    async fn secondary_activate(&self, _x: i32, _y: i32) {
        if let Some(tx) = TRAY_ACTION_TX.lock().unwrap().as_ref() {
            let _ = tx.send(TrayAction::Show);
        }
    }

    async fn scroll(&self, _delta: i32, _orientation: &str) {}
}

/// DBusMenu 菜单项节点（签名 (ia{sv}av)）
/// serde::Serialize 是 zbus interface 宏要求的
#[derive(Debug, serde::Serialize, zbus::zvariant::Type)]
#[zvariant(signature = "(ia{sv}av)")]
struct MenuNode {
    id: i32,
    properties: std::collections::HashMap<String, zbus::zvariant::OwnedValue>,
    children: Vec<zbus::zvariant::OwnedValue>,
}

#[derive(Clone)]
struct DbusMenu;

#[interface(name = "com.canonical.dbusmenu")]
impl DbusMenu {
    #[zbus(property)]
    fn version(&self) -> i32 {
        3
    }

    #[zbus(property)]
    fn text_direction(&self) -> &str {
        "ltr"
    }

    #[zbus(property)]
    fn status(&self) -> &str {
        "normal"
    }

    /// 返回菜单布局，签名必须是 (u(ia{sv}av))
    fn get_layout(
        &self,
        parent_id: i32,
        _recursion_depth: i32,
        _property_names: Vec<String>,
    ) -> (u32, MenuNode) {
        let revision: u32 = 1;
        if parent_id == 0 {
            let node = make_menu_node(0, "", vec![
                make_menu_node(1, "显示窗口", vec![]),
                make_menu_node(2, "退出", vec![]),
            ]);
            return (revision, node);
        }
        (revision, make_menu_node(parent_id, "", vec![]))
    }

    fn about_to_show(&self, _id: i32) -> bool {
        false
    }

    fn event(&self, id: i32, event_id: &str, _data: zbus::zvariant::Value<'_>, _timestamp: u32) {
        if event_id != "clicked" {
            return;
        }
        match id {
            1 => {
                log::info!("托盘菜单: 显示窗口");
                if let Some(tx) = TRAY_ACTION_TX.lock().unwrap().as_ref() {
                    let _ = tx.send(TrayAction::Show);
                }
            }
            2 => {
                log::info!("托盘菜单: 退出");
                if let Some(tx) = TRAY_ACTION_TX.lock().unwrap().as_ref() {
                    let _ = tx.send(TrayAction::Quit);
                }
            }
            _ => {}
        };
    }
}

/// 创建菜单节点
/// children 需要包成 OwnedValue (Variant) 因为 DBusMenu 签名是 av (array<variant>)
fn make_menu_node(
    id: i32,
    label: &str,
    children: Vec<MenuNode>,
) -> MenuNode {
    let mut props: std::collections::HashMap<String, zbus::zvariant::OwnedValue> =
        std::collections::HashMap::new();
    if !label.is_empty() {
        let v: zbus::zvariant::Value<'_> = zbus::zvariant::Value::from(label);
        let owned = zbus::zvariant::OwnedValue::try_from(v).unwrap();
        props.insert("label".to_string(), owned);
    }
    // 每个子 MenuNode → OwnedValue (Variant 包装的 Structure)
    let child_variants: Vec<zbus::zvariant::OwnedValue> = children
        .into_iter()
        .map(|node| {
            let s = zbus::zvariant::Structure::from((node.id, node.properties, node.children));
            let val: zbus::zvariant::Value<'_> = zbus::zvariant::Value::from(s);
            zbus::zvariant::OwnedValue::try_from(val).unwrap()
        })
        .collect();
    MenuNode { id, properties: props, children: child_variants }
}

/// 生成 24x24 应用图标像素数据
/// StatusNotifierItem 规范要求 IconPixmap 为 ARGB32，按小端（little-endian）写入时
/// 字节顺序必须是 [B, G, R, A]（即 native u32 = 0xAARRGGBB）。
/// 之前写成 [R, G, B, A] 导致图标解析异常、桌面环境用 "…" 占位。
fn generate_icon_pixmap() -> Vec<(i32, i32, Vec<u8>)> {
    let size: u32 = 24;
    let mut pixels = vec![0u8; (size * size * 4) as usize];

    // 品牌蓝 #3498db -> (R, G, B)
    let blue = (52u8, 152u8, 219u8);
    let white = (255u8, 255u8, 255u8);

    for y in 0..size {
        for x in 0..size {
            let (fx, fy) = (x as f64 + 0.5, y as f64 + 0.5);
            let idx = ((y * size + x) * 4) as usize;
            if rounded_rect_mask(fx, fy, size as f64, size as f64, 5.0) {
                // 蓝底
                set_pixel(&mut pixels, idx, blue.0, blue.1, blue.2, 255);
                // 白色蓝牙符
                if on_bluetooth_rune(fx, fy) {
                    set_pixel(&mut pixels, idx, white.0, white.1, white.2, 255);
                }
            }
        }
    }

    vec![(size as i32, size as i32, pixels)]
}

/// 以 [B, G, R, A] 顺序写入一个像素（符合 ARGB32 小端规范）
fn set_pixel(pixels: &mut [u8], idx: usize, r: u8, g: u8, b: u8, a: u8) {
    pixels[idx] = b;
    pixels[idx + 1] = g;
    pixels[idx + 2] = r;
    pixels[idx + 3] = a;
}

/// 圆角矩形遮罩
fn rounded_rect_mask(x: f64, y: f64, w: f64, h: f64, r: f64) -> bool {
    if x < 0.0 || y < 0.0 || x > w || y > h {
        return false;
    }
    let left = x < r;
    let right = x > w - r;
    let top = y < r;
    let bottom = y > h - r;
    if left && top {
        return (x - r).hypot(y - r) <= r;
    }
    if right && top {
        return (x - (w - r)).hypot(y - r) <= r;
    }
    if left && bottom {
        return (x - r).hypot(y - (h - r)) <= r;
    }
    if right && bottom {
        return (x - (w - r)).hypot(y - (h - r)) <= r;
    }
    true
}

/// 蓝牙符（白色），由三条折线组成
fn on_bluetooth_rune(x: f64, y: f64) -> bool {
    let t = 1.3;
    dist_to_segment(x, y, 12.0, 3.0, 12.0, 21.0) <= t // 垂直中线
        || dist_to_segment(x, y, 12.0, 3.0, 4.0, 12.0) <= t // 左上臂
        || dist_to_segment(x, y, 4.0, 12.0, 12.0, 21.0) <= t // 左下臂
        || dist_to_segment(x, y, 12.0, 3.0, 20.0, 12.0) <= t // 右上臂
        || dist_to_segment(x, y, 20.0, 12.0, 12.0, 21.0) <= t // 右下臂
}

fn dist_to_segment(px: f64, py: f64, ax: f64, ay: f64, bx: f64, by: f64) -> f64 {
    let dx = bx - ax;
    let dy = by - ay;
    let len2 = dx * dx + dy * dy;
    if len2 == 0.0 {
        return ((px - ax).powi(2) + (py - ay).powi(2)).sqrt();
    }
    let t = (((px - ax) * dx + (py - ay) * dy) / len2).clamp(0.0, 1.0);
    let cx = ax + t * dx;
    let cy = ay + t * dy;
    ((px - cx).powi(2) + (py - cy).powi(2)).sqrt()
}

/// StatusNotifierItem 托盘后端
pub struct SnITray {
    state: Arc<Mutex<SnIState>>,
    rt: Option<tokio::runtime::Runtime>,
    bus_name: String,
}

impl SnITray {
    pub fn new() -> Self {
        let id = std::process::id();
        let bus_name = format!("org.kde.StatusNotifierItem_{}_{}", id, id);
        Self {
            state: Arc::new(Mutex::new(SnIState {
                status: TrayState::Disconnected,
                rssi: -100,
            })),
            rt: None,
            bus_name,
        }
    }
}

impl TrayBackend for SnITray {
    fn start(&mut self) -> bool {
        let state = self.state.clone();
        let bus_name = self.bus_name.clone();

        // 单 worker 的 multi-thread runtime：spawn 的任务由后台 worker 自动驱动。
        // 不能用 current-thread runtime —— 它需要 block_on 驱动，否则托盘服务不会启动。
        let rt = match tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                log::error!("无法创建 tokio 运行时: {e}");
                return false;
            }
        };

        rt.spawn(async move {
            if let Err(e) = run_sni_service(state, &bus_name).await {
                log::error!("StatusNotifierItem D-Bus 服务启动失败: {e}");
            }
        });
        self.rt = Some(rt);
        log::info!("StatusNotifierItem 托盘已启动: {}", self.bus_name);
        true
    }

    fn update_status(&mut self, state: TrayState, rssi: i32) {
        if let Ok(mut s) = self.state.lock() {
            s.status = state;
            s.rssi = rssi;
        }
    }

    fn set_action_sender(&mut self, tx: mpsc::Sender<TrayAction>) {
        *TRAY_ACTION_TX.lock().unwrap() = Some(tx);
    }

    fn stop(&mut self) {
        if let Some(rt) = self.rt.take() {
            rt.shutdown_background();
        }
        log::info!("StatusNotifierItem 托盘已停止");
    }
}

/// 运行 SNI + DBusMenu D-Bus 服务
/// AppIndicator 硬编码期望 /StatusNotifierItem 和 /Menu，所以直接注册这两个路径。
async fn run_sni_service(
    state: Arc<Mutex<SnIState>>,
    bus_name: &str,
) -> zbus::Result<()> {
    let item = SnIItem {
        state: state.clone(),
        menu_path: "/Menu".to_string(),
    };
    let menu = DbusMenu;

    let conn = Builder::session()?
        .serve_at("/StatusNotifierItem", item)?
        .serve_at("/Menu", menu)?
        .build()
        .await?;

    // 请求 Well-Known 名称
    if let Err(e) = conn.request_name(bus_name).await {
        log::warn!("请求 D-Bus 名称 {} 失败: {}", bus_name, e);
    }

    // 注册到 StatusNotifierWatcher
    register_with_watcher(bus_name, &conn).await;

    log::info!("SNI + DBusMenu 服务运行中: /StatusNotifierItem + /Menu");

    // 保持服务运行：Runtime 由 SnITray 持有即可存活，无需每小时唤醒一次的定时器
    std::future::pending::<zbus::Result<()>>().await
}

/// 注册到 StatusNotifierWatcher
async fn register_with_watcher(bus_name: &str, conn: &zbus::Connection) {
    let proxy = match zbus::Proxy::new(
        conn,
        "org.kde.StatusNotifierWatcher",
        "/StatusNotifierWatcher",
        "org.kde.StatusNotifierWatcher",
    )
    .await
    {
        Ok(p) => p,
        Err(e) => {
            log::warn!("StatusNotifierWatcher 不可用: {e}");
            return;
        }
    };

    // RegisterStatusNotifierItem 只接受 bus_name 一个参数
    match proxy.call_method("RegisterStatusNotifierItem", &(bus_name,)).await {
        Ok(_) => log::info!("已注册到 StatusNotifierWatcher: {bus_name}"),
        Err(e) => log::warn!("注册 StatusNotifierItem 失败: {e}"),
    }
}
