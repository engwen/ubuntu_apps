mod alsa;
mod config;
mod desktop;
mod guard;
mod library;
mod lrc;
mod lyrics;
mod meta;
mod mpris;
mod online;
mod player;
mod tags;
mod ui;

use gtk::prelude::*;
use std::sync::atomic::{AtomicBool, Ordering};

/// 本进程有没有真正建成过窗口。
/// 没有的话说明它只是被已有实例接管（GApplication 单实例），不能把
/// 「上次运行标记」的账算到自己头上，否则下一次启动会误进安全模式。
static ACTIVATED: AtomicBool = AtomicBool::new(false);

fn main() {
    // 无界面自检：owlmusic --selftest [--dir 音乐目录]
    let args: Vec<String> = std::env::args().skip(1).collect();
    // 诊断单个文件能不能解码：owlmusic --probe 某首歌.flac
    if let Some(i) = args.iter().position(|a| a == "--probe") {
        match args.get(i + 1) {
            Some(f) => {
                println!("{f}");
                match player::probe_file(std::path::Path::new(f)) {
                    Ok(s) => println!("  ✓ 可以播放：{s}"),
                    Err(e) => println!("  ✗ 播不了：{e}"),
                }
            }
            None => println!("用法：owlmusic --probe <音频文件>"),
        }
        return;
    }
    if args.iter().any(|a| a == "--selftest") {
        selftest(&args);
        return;
    }
    // 无界面验证 MPRIS：注册服务、挂一首假歌、跑几秒主循环。
    // 另开一个终端用 dbus-send / playerctl 就能查到它。
    if args.iter().any(|a| a == "--mpris-selftest") {
        let secs = args
            .iter()
            .position(|a| a == "--mpris-selftest")
            .and_then(|i| args.get(i + 1))
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(5);
        mpris_selftest(secs);
        return;
    }

    // 让窗口出现在任务栏时带上应用名
    glib::set_application_name("OwlMusic");
    // X11 下 WM_CLASS 取的是程序名（默认是二进制名 owlmusic），
    // 和 desktop 里的 StartupWMClass 对不上，Dock 就会把它当成陌生程序，
    // 名字退回那串 app id。这里对齐成同一个 id。
    glib::set_prgname(Some(desktop::APP_ID));
    // 开发时直接跑二进制，系统里没有 .desktop —— 补一份到用户目录，
    // 好让 Dock 显示 OwlMusic 和图标；装过 deb 时这一步什么都不做。
    if desktop::ensure_registered() {
        guard::log("已补写用户级 desktop 条目");
    }

    // 崩了要留下证据：release 是 panic=abort，没有 hook 的话进程直接消失
    guard::install_panic_hook();
    // 上次没正常退出 -> 本次安全模式（关掉封面背景之类容易出事的重活）
    let safe = guard::begin_run();

    let app = gtk::Application::builder()
        .application_id(desktop::APP_ID)
        .build();

    app.connect_activate(move |application| {
        ACTIVATED.store(true, Ordering::Relaxed);
        guard::log("activate");
        // 窗口/Alt-Tab 的回退图标也用自家那一套（图标随 deb 装进 hicolor，
        // 开发运行时由 install-icon.sh 装到用户级 hicolor）。
        gtk::Window::set_default_icon_name(desktop::APP_ID);
        if let Some(existing) = application.active_window() {
            // 走到这里说明是「再点一次图标」：把已有窗口提到前面就行
            guard::log("已有窗口，present 后返回（没有新建实例）");
            existing.present();
            return;
        }
        let state = ui::App::new(application, safe);
        state.window.present();
    });

    let code = app.run();
    // 能走到这里说明主循环结束了；如果日志里没有 activate，多半是被
    // 另一个还没退出的实例接管了（GApplication 单实例），或者初始化就失败了
    if !ACTIVATED.load(Ordering::Relaxed) {
        // 没建成窗口的进程不拥有运行标记，交给原先的实例继续持有
        guard::abandon_run();
    }
    guard::log(&format!("主循环退出 code={}", i32::from(code)));
}

// ================= 无界面自检 =================
fn selftest(args: &[String]) {
    println!("OwlMusic {} 自检", env!("CARGO_PKG_VERSION"));
    println!("  配置目录 : {}", config::config_dir().display());
    println!("  缓存目录 : {}", config::cache_dir().display());
    println!(
        "  libasound: {}",
        if alsa::available() { "可用" } else { "缺失（装 libasound2）" }
    );
    let dir = args
        .iter()
        .position(|a| a == "--dir")
        .and_then(|i| args.get(i + 1))
        .cloned();
    let roots: Vec<std::path::PathBuf> = match dir {
        Some(d) => vec![std::path::PathBuf::from(d)],
        None => config::Config::load().roots,
    };
    if roots.is_empty() {
        println!("  曲库     : 未配置目录（用 --dir 指定一个）");
    } else {
        println!("  扫描     : {}", roots.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", "));
        let tracks = library::scan(&roots, |n, p| {
            if n % 100 == 0 {
                println!("    …{n} {}", p.file_name().unwrap_or_default().to_string_lossy());
            }
        });
        println!("  找到     : {} 首", tracks.len());
        for t in tracks.iter().take(5) {
            let m = crate::meta::read_meta_from_path(&t.path);
            println!(
                "    · {} — {} | {} | {} | 年份:{}({}) 作曲:{} | 封面:{} 歌词:{} | {}",
                t.title,
                t.display_artist(),
                t.display_album(),
                library::fmt_time(t.dur_ms),
                t.year
                    .map(|y| y.to_string())
                    .unwrap_or_else(|| "-".into()),
                m.year_marker.as_deref().unwrap_or("-"),
                if t.composer.trim().is_empty() {
                    "-"
                } else {
                    t.composer.as_str()
                },
                if t.has_cover { "有" } else { "无" },
                if t.has_lyrics { "有" } else { "无" },
                m.version.as_deref().unwrap_or("-")
            );
        }
        if let Some(t) = tracks.first() {
            match online::fetch_lyrics(&t.artist, &t.title, &t.album, t.dur_ms) {
                Ok(hit) => {
                    let l = lrc::parse(&hit.lrc);
                    println!(
                        "  在线歌词 : {} 行（{}，来自 {}）",
                        l.lines.len(),
                        if l.synced { "带时间轴" } else { "纯文本" },
                        hit.source
                    );
                }
                Err(e) => println!("  在线歌词 : 未取到（{e}）"),
            }
            match online::search_all(&format!("{} {}", t.artist, t.title), "cn") {
                Ok(v) => println!("  在线元数据: {} 条候选", v.len()),
                Err(e) => println!("  在线元数据: 失败（{e}）"),
            }
        }
    }
    println!("完成。");
}

// ================= MPRIS 自检 =================
/// 注册 MPRIS 服务、挂一首假歌，然后跑几秒主循环。
/// 期间另开终端执行
///   `dbus-send --session --print-reply --dest=org.mpris.MediaPlayer2.owlmusic \
///      /org/mpris/MediaPlayer2 org.freedesktop.DBus.Properties.GetAll \
///      string:org.mpris.MediaPlayer2.Player`
/// 或用 `playerctl -l` / `playerctl metadata` 就能看到它。
fn mpris_selftest(secs: u64) {
    use std::sync::atomic::{AtomicU32, AtomicU64, AtomicU8};
    use std::sync::{Arc, Mutex};

    println!("OwlMusic MPRIS 自检（{secs} 秒后退出）");
    let shared = Arc::new(player::Shared {
        pos_ms: AtomicU64::new(65_000),
        dur_ms: AtomicU64::new(245_000),
        state: AtomicU8::new(player::ST_PLAYING),
        volume: AtomicU32::new(0.72f32.to_bits()),
        finished: AtomicU64::new(0),
        error: Mutex::new(None),
    });
    let Some(m) = mpris::Mpris::start(shared.clone(), 0.72) else {
        println!("  ✗ 连不上 session bus：DBUS_SESSION_BUS_ADDRESS 没设置？");
        std::process::exit(1);
    };
    m.set_track(
        "自检曲目",
        "自检艺人",
        "自检专辑",
        245_000,
        std::path::Path::new("/tmp/selftest.mp3"),
        None,
    );
    println!("  已注册 {}", mpris::BUS_NAME);
    println!("  现在可以用 dbus-send / playerctl 查询；Ctrl-C 提前结束。");
    // 自检要能把「状态变化会不会广播出去」也测出来：
    // 1 秒后暂停、2 秒后继续、3 秒后换一首，外面用
    //   gdbus monitor --session --dest org.mpris.MediaPlayer2.owlmusic
    // 就能看到对应的 PropertiesChanged。
    {
        let sh = shared.clone();
        let m1 = m.clone();
        glib::timeout_add_seconds_local_once(1, move || {
            sh.state
                .store(player::ST_PAUSED, std::sync::atomic::Ordering::Relaxed);
            m1.sync();
            println!("  [自检] 已切到 暂停");
        });
    }
    {
        let sh = shared.clone();
        let m2 = m.clone();
        glib::timeout_add_seconds_local_once(2, move || {
            sh.state
                .store(player::ST_PLAYING, std::sync::atomic::Ordering::Relaxed);
            m2.sync();
            println!("  [自检] 已切到 播放");
        });
    }
    {
        let m3 = m.clone();
        glib::timeout_add_seconds_local_once(3, move || {
            m3.set_track(
                "自检曲目二",
                "自检艺人二",
                "自检专辑二",
                180_000,
                std::path::Path::new("/tmp/selftest2.mp3"),
                None,
            );
            println!("  [自检] 已换歌");
        });
    }

    let ml = glib::MainLoop::new(None, false);
    let quit = ml.clone();
    glib::timeout_add_seconds_local_once(secs as u32, move || quit.quit());
    ml.run();
    println!("完成。");
}
