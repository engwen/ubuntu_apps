/// 蓝牙扫描工具模块
/// 扫描已配对设备（标记连接状态）
use std::process::Command;

#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub mac: String,
    pub name: String,
    pub connected: bool,
}

/// 检查指定 MAC 的蓝牙设备是否已连接
pub fn is_device_connected(mac: &str) -> bool {
    let output = Command::new("bluetoothctl").args(["info", mac]).output();
    match output {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            stdout.contains("Connected: yes")
        }
        Err(_) => false,
    }
}

/// 扫描所有已配对的蓝牙设备
pub fn scan_connected_devices() -> Vec<DeviceInfo> {
    let mut devices = Vec::new();
    let output = match Command::new("bluetoothctl")
        .args(["devices", "Paired"])
        .output()
    {
        Ok(out) => out,
        Err(e) => {
            log::warn!("扫描蓝牙设备失败: {e}");
            return devices;
        }
    };
    if !output.status.success() {
        return devices;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        // 格式: "Device MAC Address Device Name"
        // 设备名可能包含空格，需要用不同的解析方式
        let line = line.trim();
        if !line.starts_with("Device ") {
            continue;
        }
        let rest = &line[7..]; // 去掉 "Device " 前缀
        // 找到MAC地址后面的空格
        if let Some(mac_end) = rest.find(' ') {
            let mac = rest[..mac_end].to_string();
            let name = rest[mac_end + 1..].trim().to_string();
            if name.is_empty() {
                continue;
            }
            let connected = is_device_connected(&mac);
            log::debug!(
                "设备 {} {} [{}]",
                mac,
                name,
                if connected { "已连接" } else { "未连接" }
            );
            devices.push(DeviceInfo {
                mac,
                name,
                connected,
            });
        }
    }
    devices
}
