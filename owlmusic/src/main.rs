mod alsa;
mod config;
mod library;
mod lrc;
mod meta;
mod online;
mod player;
mod tags;
mod ui;

use gtk::prelude::*;

fn main() {
    // 无界面自检：owlmusic --selftest [--dir 音乐目录]
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--selftest") {
        selftest(&args);
        return;
    }

    // 让窗口出现在任务栏时带上应用名
    glib::set_application_name("OwlMusic");

    let app = gtk::Application::builder()
        .application_id("io.github.owl.owlmusic")
        .build();

    app.connect_activate(|application| {
        gtk::Window::set_default_icon_name("multimedia-audio-player");
        if let Some(existing) = application.active_window() {
            existing.present();
            return;
        }
        let state = ui::App::new(application);
        state.window.present();
    });

    app.run();
}

// ================= 无界面自检 =================
// 不启动 GTK，用于在终端里确认解码器、标签、歌词、网络是否正常。
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
