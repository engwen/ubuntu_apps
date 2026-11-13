/// 蓝牙监控模块
/// 通过 BlueZ D-Bus API 监控手机蓝牙连接状态和信号强度
use std::collections::VecDeque;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;

const BLUEZ_BUS: &str = "org.bluez";
const DEVICE_IF: &str = "org.bluez.Device1";
const ADAPTER_IF: &str = "org.bluez.Adapter1";
const PROPS_IF: &str = "org.freedesktop.DBus.Properties";

/// 蓝牙监控事件
#[derive(Debug, Clone)]
pub enum BtEvent {
    Connected,
    Disconnected,
    RssiChanged(i32),
    AdapterPoweredOff,
    AdapterPoweredOn,
}

/// 蓝牙监控器
pub struct BluetoothMonitor {
    pub mac: String,
    pub rssi_threshold: i32,
    pub connected: bool,
    pub current_rssi: i32,
    pub device_path: String,
    pub adapter_powered: bool,
    pub filtered_rssi: i32,
    pub rssi_supported: bool,
    rssi_samples: VecDeque<i32>,
    // 连接状态去抖动
        conn_confirm_count: i32,
        pending_conn: Option<bool>,
        /// 稳定连接状态，确认后更新，用于 RSSI 查询
        stable_connected: bool,
    // 事件通道（与主轮询结果共用 PollMsg 通道，便于主线程统一处理）
    event_tx: Option<mpsc::Sender<crate::PollMsg>>,
}

struct DeviceProps {
    connected: Option<bool>,
    rssi: Option<i32>,
}

impl BluetoothMonitor {
    pub fn new(mac: &str, rssi_threshold: i32) -> Self {
        // BlueZ 设备对象路径里的 MAC 大小写随系统/配对时写法而定（本机实测为大写
        // 如 dev_8C_7A_3D_2D_26_FF），这里统一归一化为大写（bluetoothctl 同样接受）。
        // 真正的路径匹配在 find_device_om 中采用大小写不敏感方式，并保留 BlueZ 真实
        // 返回的路径原文，因此后续 D-Bus 调用兼容大小写两种写法。
        Self {
            mac: mac.to_uppercase().replace('-', ":"),
            rssi_threshold,
            connected: false,
            current_rssi: -100,
            device_path: String::new(),
            adapter_powered: true,
            filtered_rssi: -100,
            rssi_supported: false,
            rssi_samples: VecDeque::with_capacity(8),
            conn_confirm_count: 0,
            pending_conn: None,
            stable_connected: false,
            event_tx: None,
        }
    }

    pub fn set_event_sender(&mut self, tx: mpsc::Sender<crate::PollMsg>) {
        self.event_tx = Some(tx);
    }

    fn send_event(&self, event: BtEvent) {
        if let Some(tx) = &self.event_tx {
            let _ = tx.send(crate::PollMsg::Event(event));
        }
    }

    /// 更新 RSSI，应用滑动中位数滤波
    fn update_rssi(&mut self, raw: i32) {
        if raw == 0 || raw == -100 {
            if self.current_rssi == -100 {
                self.current_rssi = 0;
                self.filtered_rssi = 0;
                self.rssi_supported = false;
                log::warn!("适配器不支持经典蓝牙 RSSI");
            }
            return;
        }
        self.rssi_supported = true;
        self.current_rssi = raw;
        if raw > -100 {
            self.rssi_samples.push_back(raw);
            if self.rssi_samples.len() > 8 {
                self.rssi_samples.pop_front();
            }
        }
        if !self.rssi_samples.is_empty() {
            let mut sorted: Vec<i32> = self.rssi_samples.iter().copied().collect();
            sorted.sort();
            self.filtered_rssi = sorted[sorted.len() / 2];
        } else {
            self.filtered_rssi = raw;
        }
    }

    /// 初始化：查找设备路径并获取初始状态
    pub fn setup(&mut self) -> bool {
        self.device_path = self.find_device();
        if self.device_path.is_empty() {
            log::warn!("未找到设备 {}，等待适配器启动后重试", self.mac);
            return false;
        }
        let props = self.get_device_props();
        self.connected = props.connected.unwrap_or(false);
        self.stable_connected = self.connected;
        if let Some(r) = props.rssi {
            self.update_rssi(r);
        }
        log::info!(
            "蓝牙监控就绪 - 设备: {} 连接: {} RSSI: {}",
            self.device_path,
            self.connected,
            self.current_rssi
        );
        true
    }

    /// 手动轮询设备状态（作为 D-Bus 信号的补充）
    pub fn poll_status(&mut self) {
        // 检测适配器状态
        let new_powered = self.get_adapter_powered();
        if let Some(powered) = new_powered {
            if powered != self.adapter_powered {
                self.adapter_powered = powered;
                if !powered {
                    log::info!("蓝牙适配器已关闭");
                    self.send_event(BtEvent::AdapterPoweredOff);
                    return;
                } else {
                    log::info!("蓝牙适配器已开启");
                    self.send_event(BtEvent::AdapterPoweredOn);
                }
            }
        }
        if !self.adapter_powered {
            return;
        }
        if self.device_path.is_empty() {
            self.device_path = self.find_device();
            if self.device_path.is_empty() {
                // 使用 bluetoothctl 兜底（连接状态变化由轮询循环统一上报，这里只更新内部状态）
                let connected = self.get_connected_via_bluetoothctl();
                if let Some(c) = connected {
                    if c != self.connected {
                        self.connected = c;
                    }
                }
                return;
            }
        }
        let props = self.get_device_props();
        let new_connected = props.connected.unwrap_or(false);
        // 去抖动处理（仅更新内部状态，连接/断开事件由轮询主循环统一上报，避免重复）
        if new_connected == self.connected {
            self.pending_conn = None;
            self.conn_confirm_count = 0;
        } else {
            if Some(new_connected) == self.pending_conn {
                self.conn_confirm_count += 1;
            } else {
                self.pending_conn = Some(new_connected);
                self.conn_confirm_count = 1;
            }
            let threshold = if new_connected { 3 } else { 2 };
            if self.conn_confirm_count >= threshold {
                let old = self.connected;
                self.connected = new_connected;
                self.pending_conn = None;
                self.conn_confirm_count = 0;
                log::info!("[轮询] 连接状态确认变化: {old} -> {new_connected}");
            }
        }
        // RSSI 只在设备已连接且 BlueZ 暴露了真实值时读取，避免断开后读到虚假值
        if self.connected {
            match props.rssi {
                Some(raw) => {
                    self.update_rssi(raw);
                }
                None => {
                    // BlueZ 未暴露 RSSI（现代 BlueZ 对已连接设备常如此）。
                    // 必须如实标记不支持，否则 -100 会被当成“低于阈值”而误触发锁屏。
                    self.rssi_supported = false;
                }
            }
            self.stable_connected = true;
        } else {
            // 断开状态稳定后重置 RSSI
            if self.stable_connected {
                self.current_rssi = -100;
                self.filtered_rssi = -100;
                self.rssi_samples.clear();
                self.stable_connected = false;
            }
        }
    }

    /// 启动连接子进程（LE + 经典蓝牙双路并行），返回 (le_proc, br_proc)
    pub fn start_connect_process(&self, timeout_sec: u64) -> (Option<Child>, Option<Child>) {
        let mac = self.mac.clone();
        log::info!("启动连接子进程: {mac}（超时{timeout_sec}秒）");
        // 方式1: bluetoothctl connect（LE）
        let le_proc = Command::new("timeout")
            .arg(format!("{timeout_sec}"))
            .args(["bluetoothctl", "connect", &mac])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .ok();
        // 方式2: gdbus Device1.Connect（经典蓝牙）
        let dbus_path = format!("/org/bluez/hci0/dev_{}", mac.replace(':', "_"));
        let br_proc = Command::new("timeout")
            .arg(format!("{}", timeout_sec + 5))
            .args([
                "gdbus",
                "call",
                "--system",
                "--dest",
                BLUEZ_BUS,
                "--object-path",
                &dbus_path,
                "--method",
                &format!("{DEVICE_IF}.Connect"),
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .ok();
        (le_proc, br_proc)
    }

    /// 实时查询设备连接状态
    pub fn check_connected_now(&mut self) -> bool {
        let props = self.get_device_props();
        props.connected.unwrap_or(false)
    }

    /// 是否正在经历状态变化（去抖动进行中，尚未确认）
    pub fn is_state_changing(&self) -> bool {
        self.pending_conn.is_some() && self.conn_confirm_count > 0
    }

    /// 断开指定蓝牙外设
    pub fn disconnect_peripheral(&self, mac: &str) -> bool {
        let path_suffix = mac.to_uppercase().replace(':', "_");
        let device_path = format!("/org/bluez/hci0/dev_{path_suffix}");
        let result = Command::new("gdbus")
            .args([
                "call",
                "--system",
                "--dest",
                BLUEZ_BUS,
                "--object-path",
                &device_path,
                "--method",
                &format!("{DEVICE_IF}.Disconnect"),
            ])
            .output();
        match result {
            Ok(out) => {
                if out.status.success() {
                    log::info!("已断开外设: {mac}");
                    true
                } else {
                    false
                }
            }
            Err(e) => {
                log::error!("断开外设异常: {e}");
                false
            }
        }
    }

    // ── 内部方法 ──────────────────────────────────────────────────────────────

    fn find_device(&self) -> String {
        let path_suffix = self.mac.replace(':', "_");
        let candidate = format!("/org/bluez/hci0/dev_{path_suffix}");
        let output = Command::new("gdbus")
            .args([
                "call",
                "--system",
                "--dest",
                BLUEZ_BUS,
                "--object-path",
                &candidate,
                "--method",
                &format!("{PROPS_IF}.GetAll"),
                DEVICE_IF,
            ])
            .output();
        if let Ok(output) = output {
            if output.status.success() {
                return candidate;
            }
        }
        self.find_device_om()
    }

    fn find_device_om(&self) -> String {
        let output = Command::new("gdbus")
            .args([
                "call",
                "--system",
                "--dest",
                BLUEZ_BUS,
                "--object-path",
                "/",
                "--method",
                "org.freedesktop.DBus.ObjectManager.GetManagedObjects",
            ])
            .output();
        if let Ok(out) = output {
            if out.status.success() {
                let stdout = String::from_utf8_lossy(&out.stdout);
                let path_suffix = self.mac.replace(':', "_");
                let target = format!("dev_{path_suffix}").to_lowercase();
                for line in stdout.lines() {
                    if line.to_lowercase().contains(&target) {
                        // 提取路径
                        if let Some(start) = line.find("'/org/bluez/") {
                            let rest = &line[start + 1..];
                            if let Some(end) = rest.find('\'') {
                                return rest[..end].to_string();
                            }
                        }
                    }
                }
            }
        }
        String::new()
    }

    fn get_device_props(&self) -> DeviceProps {
        let mut props = DeviceProps {
            connected: None,
            rssi: None,
        };
        // 通过 BlueZ D-Bus GetAll 同时读取 Connected 与 RSSI
        // 不再依赖 hcitool（BlueZ ≥ 5.66 / Ubuntu 24.04 已移除）
        if let Some(raw) = self.get_device_props_raw() {
            props.connected = Self::parse_connected(&raw);
            props.rssi = Self::parse_rssi(&raw);
        }
        // bluetoothctl 读的同样是 BlueZ 的 D-Bus 状态，正常情况下无需再查一遍。
        // 仅在 D-Bus 解析失败时兜底，避免每轮轮询多 fork/exec 一个进程。
        if props.connected.is_none() {
            props.connected = self.get_connected_via_bluetoothctl();
        }
        props
    }

    /// 读取设备全部属性（GetAll）的原始文本
    fn get_device_props_raw(&self) -> Option<String> {
        if self.device_path.is_empty() {
            return None;
        }
        let output = Command::new("gdbus")
            .args([
                "call",
                "--system",
                "--dest",
                BLUEZ_BUS,
                "--object-path",
                &self.device_path,
                "--method",
                &format!("{PROPS_IF}.GetAll"),
                DEVICE_IF,
            ])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        Some(String::from_utf8_lossy(&output.stdout).to_string())
    }

    fn parse_connected(raw: &str) -> Option<bool> {
        if raw.contains("'Connected': <true>") || raw.contains("\"Connected\": <true>") {
            Some(true)
        } else if raw.contains("'Connected': <false>") || raw.contains("\"Connected\": <false>") {
            Some(false)
        } else {
            None
        }
    }

    /// 解析 BlueZ Device1.RSSI 属性（D-Bus GetAll 输出）
    /// 兼容 'RSSI': < -55 > 与 "RSSI": < -55 > 两种引号风格
    fn parse_rssi(raw: &str) -> Option<i32> {
        let marker = raw.find("'RSSI'").or_else(|| raw.find("\"RSSI\""))?;
        let s = &raw[marker..];
        let open = s.find('<')?;
        let close = s[open..].find('>')?;
        let val = s[open + 1..open + close].trim();
        val.parse::<i32>().ok()
    }

    fn get_connected_via_bluetoothctl(&self) -> Option<bool> {
        let mac = self.mac.clone();
        let output = Command::new("bluetoothctl").args(["info", &mac]).output();
        match output {
            Ok(out) => {
                if !out.status.success() {
                    return None;
                }
                let stdout = String::from_utf8_lossy(&out.stdout);
                if stdout.contains("Connected: yes") {
                    Some(true)
                } else if stdout.contains("Connected: no") {
                    Some(false)
                } else {
                    None
                }
            }
            Err(_) => None,
        }
    }

    fn get_adapter_powered(&self) -> Option<bool> {
        let adapter_path = self.get_adapter_path();
        let output = Command::new("gdbus")
            .args([
                "call",
                "--system",
                "--dest",
                BLUEZ_BUS,
                "--object-path",
                &adapter_path,
                "--method",
                &format!("{PROPS_IF}.GetAll"),
                ADAPTER_IF,
            ])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        if stdout.contains("'Powered': <true>") || stdout.contains("\"Powered\": <true>") {
            Some(true)
        } else if stdout.contains("'Powered': <false>") || stdout.contains("\"Powered\": <false>") {
            Some(false)
        } else {
            None
        }
    }

    fn get_adapter_path(&self) -> String {
        if !self.device_path.is_empty() {
            if let Some(idx) = self.device_path.find("/dev_") {
                return self.device_path[..idx].to_string();
            }
        }
        "/org/bluez/hci0".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_rssi_single_quotes() {
        let raw = "({ 'Address': <'AA:BB:CC:DD:EE:FF'>, 'Connected': <true>, 'RSSI': < -55 >, 'Name': <'Phone'> },)";
        assert_eq!(BluetoothMonitor::parse_rssi(raw), Some(-55));
    }

    #[test]
    fn parse_rssi_double_quotes() {
        let raw = "( { \"Connected\": <false>, \"RSSI\": < -42 > }, )";
        assert_eq!(BluetoothMonitor::parse_rssi(raw), Some(-42));
    }

    #[test]
    fn parse_rssi_absent() {
        let raw = "({ 'Connected': <true> },)";
        assert_eq!(BluetoothMonitor::parse_rssi(raw), None);
    }

    #[test]
    fn parse_connected_variants() {
        assert_eq!(BluetoothMonitor::parse_connected("'Connected': <true>"), Some(true));
        assert_eq!(BluetoothMonitor::parse_connected("\"Connected\": <false>"), Some(false));
        assert_eq!(BluetoothMonitor::parse_connected("no connected field"), None);
    }
}
