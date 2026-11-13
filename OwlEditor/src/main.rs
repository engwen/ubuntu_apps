//! OwlEditor — Linux 原生大文件文本编辑器（Rust + GTK4）。
//!
//! 定位：对标 Kate / EmEditor / Notepad-- 的菜单布局与基础功能，但
//! 不依赖 KDE 运行环境（Kate >200 MiB 依赖）、打开大文件不卡（Notepad--
//! 会卡）、Linux 原生（EmEditor 无 Linux 版）。
//!
//! **200 MiB 是内存预算上限，不是优化目标。** 为了精确行号、可点击的
//! 滚动条、可靠的编辑与跳转，程序会用几 MiB 建一份全文件行索引；
//! 文件越大占用越大是正常的，但打开 1 KiB 小文件不会平白占 200 MiB。
//!
//! ## 关键实现位置
//!
//! * 打开即时可用 + 内存有界
//!   → `owl-core/src/chunks.rs`（固定 8 MiB 块缓存）、`pieces.rs`（编辑只存增量）
//! * 精确行号 / 滚动条 / 跳转（消除重复行）
//!   → `owl-core/src/index.rs`（后台渐进式全文件行索引，行号↔字节精确互换）
//! * Wayland 原生 / TextInput v3 / ibus-rime 直接输入中文 / 无环境变量 hack
//!   → `src/ui/ime.rs`
//! * 仿 Kate 的菜单 / 工具栏 / 状态栏
//!   → `src/ui/window.rs`（GMenu + GAction + GtkPopoverMenuBar）
//! * 一窗多标签 / 未命名缓冲区 / 各标签独立的查找栏与滚动条
//!   → `src/ui/tab.rs`
//! * 虚拟自绘视图、双向滚动、自动换行、选区
//!   → `src/ui/view.rs`、`src/ui/state.rs`
//! * 全窗口统一配色
//!   → `src/ui/theme.rs`
//! * UTF-8 优先 / 流式查找 / 原子保存 / 打印
//!   → `owl-core/src/utf8.rs`、`search.rs`、`document.rs`

mod ui;

use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use std::path::PathBuf;

/// 崩溃留证。
///
/// GTK 信号回调里的 panic 无法展开到 FFI 之外，进程会直接 abort —— 从桌面
/// 启动时看不到任何输出，用户只能报「闪退」。这里先把 panic 信息与 backtrace
/// 落到 `~/.cache/owleditor/crash.log`，事后能定位到具体行。
fn install_crash_log() {
    std::panic::set_hook(Box::new(|info| {
        let bt = std::backtrace::Backtrace::force_capture();
        let text = format!("{info}\n\n{bt}\n");
        eprintln!("{text}");

        let dir = std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
            .map(|d| d.join("owleditor"));
        let Some(dir) = dir else { return; };
        let _ = std::fs::create_dir_all(&dir);
        let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("crash.log"))
        else {
            return;
        };
        let ts = glib::DateTime::now_local()
            .ok()
            .and_then(|d| d.format("%Y-%m-%d %H:%M:%S").ok())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "未知时间".into());
        use std::io::Write;
        let _ = writeln!(f, "===== {ts} =====");
        let _ = f.write_all(text.as_bytes());
    }));
}

fn main() -> glib::ExitCode {
    install_crash_log();

    // GLib 默认从 argv[0] 推导应用名（这里是小写的 "owleditor"）。
    // 桌面项未被 dock 关联上时，任务栏会退回显示这个名字，看起来就是
    // 「名称不对」。显式设成正式产品名，与窗口标题、.desktop 的 Name 一致。
    glib::set_application_name(ui::state::APP_NAME);

    // HANDLES_OPEN：命令行给的文件走 `open` 信号（可一次给多个，一个文件一个
    // 标签页）；第二个实例打开文件时也会通过 D-Bus 交给已运行的实例。
    let app = gtk::Application::builder()
        .application_id(ui::state::APP_ID)
        .flags(gio::ApplicationFlags::HANDLES_OPEN)
        .build();

    // 没有文件：开一个窗口 + 一个未命名缓冲区，直接就能打字。
    app.connect_activate(|a| {
        if let Some(w) = a.active_window() {
            w.present();
            return;
        }
        ui::open_window(a, Vec::new());
    });

    // 带文件启动：开新窗口，每个文件一个标签页（不打断已有窗口的布局）。
    app.connect_open(|a, files, _hint| {
        let paths: Vec<PathBuf> = files.iter().filter_map(|f| f.path()).collect();
        ui::open_window(a, paths);
    });

    // 第一个参数固定为正式产品名：GApplication 从 argv[0] 推导 prgname，
    // 传二进制路径时会退回小写的 "owleditor"（dock 名字看起来就不对）。
    // 其余参数原样交给 HANDLES_OPEN 分支，一个文件一个标签页。
    let mut argv: Vec<String> = vec![ui::state::APP_NAME.to_string()];
    argv.extend(std::env::args().skip(1));
    app.run_with_args(&argv)
}
