//! 运行日志与安全模式。
//!
//! release 编译是 `panic = "abort"`，崩溃时进程「一声不响地消失」，
//! 什么线索都不留。这里做三件事：
//!
//! 1. 把启动 / 激活 / 扫描 / 退出写进 `~/.cache/owlmusic/run.log`；
//! 2. 装 panic hook，把 panic 信息和回溯也写进去；
//! 3. 运行期间留一个 `running` 标记，正常关闭时删掉。下次启动如果还看到它，
//!    说明上次没走完退出流程（崩溃 / 被 kill / 初始化就挂），本次进安全模式：
//!    临时关掉封面背景与跟随封面这类容易出事的重活，至少让窗口能起来。

use std::io::Write;
use std::path::PathBuf;

/// 日志超过这个大小就轮换一次，避免长期运行把磁盘写满。
const MAX_LOG: u64 = 512 * 1024;

fn log_path() -> PathBuf {
    crate::config::cache_dir().join("run.log")
}

fn now_text() -> String {
    glib::DateTime::now_local()
        .ok()
        .and_then(|d| d.format("%Y-%m-%d %H:%M:%S").ok())
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            // 极端情况下（没有本地时区）退化成 epoch 秒，至少还能看出先后
            let secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            secs.to_string()
        })
}

/// 追加一行日志。任何失败都静默——日志本身绝不能再把程序搞崩。
///
/// 同时回显到 stdout：从终端启动时能直接看到，桌面启动时则进 journal。
pub fn log(msg: &str) {
    // 用 writeln! 而不是 println!：stdout 被管道提前关掉时 println! 会 panic
    let mut out = std::io::stdout();
    for line in msg.lines() {
        let _ = writeln!(out, "[owlmusic] {line}");
    }
    log_to(&log_path(), msg);
}

fn log_to(path: &std::path::Path, msg: &str) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    // 太大就留后半段，避免无限增长
    if let Ok(md) = std::fs::metadata(path) {
        if md.len() > MAX_LOG {
            if let Ok(data) = std::fs::read(path) {
                let mut keep = data.len() / 2;
                // 从中间往后找字符边界，别把一行 UTF-8 从汉字的第三个字节劈开
                while keep < data.len() && (data[keep] & 0xC0) == 0x80 {
                    keep += 1;
                }
                let _ = std::fs::write(path, &data[keep..]);
            }
        }
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "{}  {}", now_text(), msg);
    }
}

/// 装 panic hook：先把现场写进 run.log，再交回默认处理（release 下即 abort）。
pub fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let backtrace = std::backtrace::Backtrace::force_capture();
        log(&format!("!! panic {info}\n{backtrace}"));
        previous(info);
    }));
}

/// 开始一次运行。返回 true 表示上次是异常退出，本次应进安全模式。
///
/// 标记里记的是进程号：如果标记存在但那个进程还活着，说明是「再点一次图标」
/// 唤起已有实例，既不该进安全模式，也不能把对方的标记抢过来（否则对方真崩了
/// 就检测不到）。只有标记对应的进程已经不在，才算上次没走完退出流程。
pub fn begin_run() -> bool {
    begin_run_in(&crate::config::cache_dir())
}

fn begin_run_in(dir: &std::path::Path) -> bool {
    let _ = std::fs::create_dir_all(dir);
    let mark = dir.join("running");
    let log = dir.join("run.log");

    let record = std::fs::read_to_string(&mark).ok();
    let previous = match record.as_deref().and_then(|s| s.trim().parse::<u32>().ok()) {
        // 已有实例在跑：不接管标记、不写自己的，也不进安全模式
        Some(pid) if pid != std::process::id() && pid_alive(pid) => return false,
        // 标记指向的进程已经没了（或内容读不出）：上次异常退出
        _ if mark.exists() => true,
        _ => false,
    };

    log_to(
        &log,
        if previous {
            "启动（上次异常退出）"
        } else {
            "启动"
        },
    );

    let _ = std::fs::write(&mark, std::process::id().to_string());
    previous
}

/// 正常退出：删掉自己留下的运行标记，下次不再进安全模式。
pub fn mark_clean() {
    remove_own_marker(&crate::config::cache_dir());
    log("正常退出");
}

/// 只是被已有实例接管（GApplication 单实例）：不发窗口的那一方不该碰标记。
pub fn abandon_run() {
    remove_own_marker(&crate::config::cache_dir());
    log("本次由已有实例接管，未新建窗口");
}

fn remove_own_marker(dir: &std::path::Path) {
    let mark = dir.join("running");
    let mine = std::fs::read_to_string(&mark)
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
        == Some(std::process::id());
    if mine {
        let _ = std::fs::remove_file(&mark);
    }
}

/// Linux 上用 /proc 判断进程是否还在（本项目只跑 Linux）。
fn pid_alive(pid: u32) -> bool {
    std::path::Path::new(&format!("/proc/{pid}")).exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 目录不存在时要自己建；写进去的内容要能读回来。
    #[test]
    fn log_creates_file_and_appends() {
        let dir = std::env::temp_dir().join(format!("owlmusic-guard-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let p = dir.join("nested").join("run.log");
        log_to(&p, "hello");
        log_to(&p, "world");
        let s = std::fs::read_to_string(&p).unwrap();
        assert!(s.contains("hello"));
        assert!(s.contains("world"));
        // 每次都是一行
        assert_eq!(s.lines().count(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fresh_start_is_not_safe_mode() {
        let d = std::env::temp_dir().join(format!("owlmusic-guard-fresh-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        assert!(!begin_run_in(&d));
        let marker = std::fs::read_to_string(d.join("running")).unwrap();
        assert_eq!(marker.trim(), std::process::id().to_string());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn dead_pid_means_previous_crash() {
        let d = std::env::temp_dir().join(format!("owlmusic-guard-dead-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        // 这个 pid 基本不可能存在（超过 pid_max 常见上限）
        std::fs::write(d.join("running"), "4294967290").unwrap();
        assert!(begin_run_in(&d));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn alive_other_pid_is_a_second_launch_not_a_crash() {
        let d = std::env::temp_dir().join(format!("owlmusic-guard-alive-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        // pid 1 一定活着；用它冒充「已经在跑的另一个实例」
        std::fs::write(d.join("running"), "1").unwrap();
        assert!(!begin_run_in(&d));
        // 不能把别人的标记覆盖成自己的
        assert_eq!(
            std::fs::read_to_string(d.join("running")).unwrap().trim(),
            "1"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn clean_exit_removes_only_own_marker() {
        let d = std::env::temp_dir().join(format!("owlmusic-guard-clean-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("running"), "1").unwrap();
        remove_own_marker(&d);
        assert!(d.join("running").exists(), "别人的标记不能删");

        std::fs::write(d.join("running"), std::process::id().to_string()).unwrap();
        remove_own_marker(&d);
        assert!(!d.join("running").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn timestamp_is_not_empty() {
        assert!(!now_text().is_empty());
    }

    /// 超过上限时轮换，且保留的是后半段（最新的日志）。
    #[test]
    fn log_rotates_when_too_large() {
        let dir = std::env::temp_dir().join(format!("owlmusic-guard-rot-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let p = dir.join("run.log");
        std::fs::create_dir_all(&dir).unwrap();
        let big = "A".repeat(MAX_LOG as usize + 10);
        std::fs::write(&p, &big).unwrap();
        log_to(&p, "tail-marker");
        let s = std::fs::read_to_string(&p).unwrap();
        assert!(s.len() < MAX_LOG as usize);
        assert!(s.contains("tail-marker"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
