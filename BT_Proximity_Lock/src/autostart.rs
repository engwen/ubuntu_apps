/// 开机自启管理模块
/// 通过 systemd user service 实现开机自启动
use std::fs;
use std::process::Command;

const SERVICE_NAME: &str = "bt-proximity-lock";

fn service_dir() -> std::path::PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("systemd")
        .join("user")
}

fn service_file() -> std::path::PathBuf {
    service_dir().join(format!("{SERVICE_NAME}.service"))
}

/// 创建并启用 systemd user service
pub fn enable_autostart() -> bool {
    let dir = service_dir();
    if let Err(e) = fs::create_dir_all(&dir) {
        log::error!("创建 service 目录失败: {e}");
        return false;
    }
    // 直接以当前运行的可执行文件路径作为启动命令（本项目是 Rust 二进制，
    // 而非 python 模块）。回退到常见安装路径。
    let exe_path = std::env::current_exe()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "/usr/bin/bt-proximity-lock".into());
    let home_dir = dirs::home_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| "/home".into());
    let content = format!(
        r#"[Unit]
Description=BT Proximity Lock - Bluetooth Proximity Auto Lock
After=graphical-session.target

[Service]
Type=simple
ExecStart={exe_path}
WorkingDirectory={home_dir}
Restart=on-failure
RestartSec=5
Environment=DISPLAY=:0
Environment=XDG_RUNTIME_DIR=/run/user/%U

[Install]
WantedBy=default.target
"#
    );
    if let Err(e) = fs::write(service_file(), content) {
        log::error!("写入 service 文件失败: {e}");
        return false;
    }
    let _ = Command::new("systemctl").args(["--user", "daemon-reload"]).output();
    let _ = Command::new("systemctl").args(["--user", "enable", SERVICE_NAME]).output();
    log::info!("开机自启已启用: {}", service_file().display());
    true
}

/// 禁用并删除 systemd user service
pub fn disable_autostart() -> bool {
    let _ = Command::new("systemctl")
        .args(["--user", "disable", SERVICE_NAME])
        .output();
    if service_file().exists() {
        let _ = fs::remove_file(service_file());
    }
    let _ = Command::new("systemctl").args(["--user", "daemon-reload"]).output();
    log::info!("开机自启已禁用");
    true
}

/// 检查开机自启是否已启用
pub fn is_autostart_enabled() -> bool {
    service_file().exists()
}
