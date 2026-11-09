/// 屏幕锁定模块
/// 支持多种桌面环境（GNOME/KDE/Deepin/XFCE/Cinnamon/MATE）的锁屏
use std::process::Command;

/// 获取系统空闲时间（毫秒），自动探测桌面环境
/// 返回 None 表示无法检测（应视为用户未活动，允许锁屏）
/// 返回 Some(0) 表示用户刚刚有过操作（鼠标/键盘）
pub fn get_idle_ms() -> Option<u64> {
    // 方式1: GNOME Mutter IdleMonitor D-Bus（Wayland + GNOME）
    if let Some(ms) = try_idle_mutter() {
        return Some(ms);
    }
    // 方式2: xprintidle（X11，需安装）
    if let Some(ms) = try_idle_xprintidle() {
        return Some(ms);
    }
    // 方式3: xssstate（X11 轻量工具）
    if let Some(ms) = try_idle_xssstate() {
        return Some(ms);
    }
    // 方式4: D-Bus org.freedesktop.ScreenSaver（通用）
    if let Some(ms) = try_idle_dbus_screensaver() {
        return Some(ms);
    }
    None
}

fn try_idle_mutter() -> Option<u64> {
    let output = Command::new("gdbus")
        .args([
            "call", "--session", "--dest", "org.gnome.Mutter.IdleMonitor",
            "--object-path", "/org/gnome/Mutter/IdleMonitor/Core",
            "--method", "org.gnome.Mutter.IdleMonitor.GetIdletime",
        ])
        .output()
        .ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    for part in stdout.split_whitespace() {
        if let Ok(ms) = part.parse::<u64>() {
            return Some(ms);
        }
    }
    None
}

fn try_idle_xprintidle() -> Option<u64> {
    let output = Command::new("xprintidle").output().ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout.trim().parse::<u64>().ok()
}

fn try_idle_xssstate() -> Option<u64> {
    let output = Command::new("xssstate").args(["-i"]).output().ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout.trim().parse::<u64>().ok()
}

fn try_idle_dbus_screensaver() -> Option<u64> {
    let output = Command::new("dbus-send")
        .args([
            "--session", "--type=method_call", "--print-reply",
            "--dest=org.freedesktop.ScreenSaver",
            "/org/freedesktop/ScreenSaver",
            "org.freedesktop.ScreenSaver.GetIdleTime",
        ])
        .output()
        .ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    for part in stdout.split_whitespace() {
        if let Ok(ms) = part.parse::<u64>() {
            return Some(ms);
        }
    }
    None
}

/// 锁定屏幕，自动探测桌面环境并选择最佳方式
pub fn lock_screen() -> bool {
    let desktop = std::env::var("XDG_CURRENT_DESKTOP")
        .unwrap_or_default()
        .to_lowercase();

    let methods: Vec<fn() -> bool> = if desktop.contains("kde") || desktop.contains("plasma") {
        vec![try_kde_lock, try_loginctl, try_xdg_screensaver]
    } else if desktop.contains("deepin") || desktop.contains("dde") {
        vec![try_deepin_lock, try_loginctl, try_gnome_screensaver]
    } else if desktop.contains("xfce") {
        vec![try_xfce_lock, try_loginctl, try_xdg_screensaver]
    } else if desktop.contains("cinnamon") {
        vec![try_cinnamon_lock, try_loginctl, try_gnome_screensaver]
    } else if desktop.contains("mate") {
        vec![try_mate_lock, try_loginctl, try_gnome_screensaver]
    } else {
        vec![try_loginctl, try_gnome_screensaver, try_xdg_screensaver]
    };

    for method in methods {
        if method() {
            return true;
        }
    }

    log::error!("所有锁屏方式均失败，请检查系统环境");
    false
}

/// 解锁当前登录会话
pub fn unlock_session() -> bool {
    let desktop = std::env::var("XDG_CURRENT_DESKTOP")
        .unwrap_or_default()
        .to_lowercase();

    // 通用: loginctl unlock-session（所有 systemd 桌面环境）
    if run_cmd(&["loginctl", "unlock-session"]) {
        log::info!("会话已解锁 (loginctl)");
        return true;
    }

    // KDE 专用解锁
    if desktop.contains("kde") || desktop.contains("plasma") {
        if run_cmd(&["qdbus", "org.kde.screensaver", "/ScreenSaver", "Unlock"]) {
            log::info!("会话已解锁 (KDE)");
            return true;
        }
        if run_cmd(&[
            "dbus-send", "--session", "--type=method_call",
            "--dest=org.kde.screensaver", "/ScreenSaver",
            "org.kde.screensaver.Unlock",
        ]) {
            log::info!("会话已解锁 (KDE D-Bus)");
            return true;
        }
    }

    // Deepin 专用解锁
    if desktop.contains("deepin") || desktop.contains("dde") {
        if run_cmd(&["dde-screenlock", "-a"]) {
            log::info!("会话已解锁 (Deepin)");
            return true;
        }
    }

    // GNOME Screensaver D-Bus Unlock
    if run_cmd(&[
        "dbus-send", "--session", "--type=method_call",
        "--dest=org.gnome.ScreenSaver", "/org/gnome/ScreenSaver",
        "org.gnome.ScreenSaver.Unlock",
    ]) {
        log::info!("会话已解锁 (gnome-screensaver)");
        return true;
    }

    log::debug!("解锁失败：所有方式均不可用");
    false
}

fn run_cmd(cmd: &[&str]) -> bool {
    match Command::new(cmd[0]).args(&cmd[1..]).output() {
        Ok(out) => out.status.success(),
        Err(_) => false,
    }
}

fn try_loginctl() -> bool {
    if run_cmd(&["loginctl", "lock-session"]) {
        log::info!("屏幕已锁定 (loginctl)");
        return true;
    }
    false
}

fn try_gnome_screensaver() -> bool {
    if run_cmd(&[
        "dbus-send", "--session", "--type=method_call",
        "--dest=org.gnome.ScreenSaver", "/org/gnome/ScreenSaver",
        "org.gnome.ScreenSaver.Lock",
    ]) {
        log::info!("屏幕已锁定 (gnome-screensaver)");
        return true;
    }
    false
}

fn try_xdg_screensaver() -> bool {
    if run_cmd(&["xdg-screensaver", "lock"]) {
        log::info!("屏幕已锁定 (xdg-screensaver)");
        return true;
    }
    false
}

fn try_kde_lock() -> bool {
    if run_cmd(&["qdbus", "org.kde.screensaver", "/ScreenSaver", "Lock"]) {
        log::info!("屏幕已锁定 (KDE)");
        return true;
    }
    if run_cmd(&[
        "dbus-send", "--session", "--type=method_call",
        "--dest=org.kde.screensaver", "/ScreenSaver",
        "org.kde.screensaver.Lock",
    ]) {
        log::info!("屏幕已锁定 (KDE D-Bus)");
        return true;
    }
    false
}

fn try_deepin_lock() -> bool {
    if run_cmd(&["dde-screenlock"]) {
        log::info!("屏幕已锁定 (Deepin)");
        return true;
    }
    false
}

fn try_xfce_lock() -> bool {
    if run_cmd(&["xfce4-screensaver-command", "--lock"]) {
        log::info!("屏幕已锁定 (XFCE)");
        return true;
    }
    false
}

fn try_cinnamon_lock() -> bool {
    if run_cmd(&[
        "dbus-send", "--session", "--type=method_call",
        "--dest=org.cinnamon.ScreenSaver", "/org/cinnamon/ScreenSaver",
        "org.cinnamon.ScreenSaver.Lock",
    ]) {
        log::info!("屏幕已锁定 (Cinnamon)");
        return true;
    }
    false
}

fn try_mate_lock() -> bool {
    if run_cmd(&["mate-screensaver-command", "--lock"]) {
        log::info!("屏幕已锁定 (MATE)");
        return true;
    }
    false
}
