//! 桌面身份：app id、Dock 名称与图标、用户级 `.desktop` 条目。
//!
//! 装了 deb 时系统里已经有 `/usr/share/applications/io.github.owl.owlmusic.desktop`，
//! 什么都不用做；直接跑 `target/debug/owlmusic` 时系统里没有条目，Dock 只能拿
//! app id 当名字（显示成一串带点的标识），窗口/Alt-Tab 也可能没有图标。
//! 这时补一份到 `~/.local/share/applications/`，并把图标写进用户图标主题。
//!
//! 条目里带 `X-OwlMusic-Auto=true` 标记，方便以后装了 deb 时只删掉我们自己
//! 写的这一份，不碰用户手动安装的条目。

use std::path::{Path, PathBuf};

/// GTK 应用 id。窗口 `app_id`、desktop 文件名、图标名、`StartupWMClass` 全用它。
pub const APP_ID: &str = "io.github.owl.owlmusic";

/// 正式图标（深靛底 + 斜置的八分音符）。编译时嵌进来，运行时才有东西可写到用户图标目录。
const ICON_SVG: &str = include_str!("../resources/io.github.owl.owlmusic.svg");

/// 同一张图的各种位图尺寸。
///
/// 只写 SVG 是不够的：hicolor 主题查找时先按尺寸找 `<N>x<N>/apps/`、找不到才
/// 退回 `scalable/`。用户目录里若残留着旧版本留下的 PNG，它会在固定尺寸这一步
/// 赢过新的 SVG，Dock 就永远显示旧图（换图标后最容易踩的坑）。
/// 所以位图也一起嵌进来，启动时按内容比对覆盖。
const ICON_PNGS: &[(u32, &[u8])] = &[
    (
        16,
        include_bytes!("../resources/icons/hicolor/16x16/apps/io.github.owl.owlmusic.png"),
    ),
    (
        24,
        include_bytes!("../resources/icons/hicolor/24x24/apps/io.github.owl.owlmusic.png"),
    ),
    (
        32,
        include_bytes!("../resources/icons/hicolor/32x32/apps/io.github.owl.owlmusic.png"),
    ),
    (
        48,
        include_bytes!("../resources/icons/hicolor/48x48/apps/io.github.owl.owlmusic.png"),
    ),
    (
        64,
        include_bytes!("../resources/icons/hicolor/64x64/apps/io.github.owl.owlmusic.png"),
    ),
    (
        128,
        include_bytes!("../resources/icons/hicolor/128x128/apps/io.github.owl.owlmusic.png"),
    ),
    (
        256,
        include_bytes!("../resources/icons/hicolor/256x256/apps/io.github.owl.owlmusic.png"),
    ),
    (
        512,
        include_bytes!("../resources/icons/hicolor/512x512/apps/io.github.owl.owlmusic.png"),
    ),
];

const AUTO_MARKER: &str = "X-OwlMusic-Auto=true";

fn data_home() -> PathBuf {
    std::env::var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into())).join(".local/share")
        })
}

fn system_entry() -> PathBuf {
    Path::new("/usr/share/applications").join(format!("{APP_ID}.desktop"))
}

fn user_entry() -> PathBuf {
    data_home()
        .join("applications")
        .join(format!("{APP_ID}.desktop"))
}

fn user_icon() -> PathBuf {
    data_home()
        .join("icons/hicolor/scalable/apps")
        .join(format!("{APP_ID}.svg"))
}

fn user_png(size: u32) -> PathBuf {
    data_home()
        .join(format!("icons/hicolor/{size}x{size}/apps"))
        .join(format!("{APP_ID}.png"))
}

/// 确保桌面条目存在。返回 true 表示这次确实补写了（调用方据此记日志）。
pub fn ensure_registered() -> bool {
    let entry = user_entry();

    // 系统版优先：装过 deb 就把自己补的条目删掉，免得旧路径遮住系统版本
    if system_entry().exists() {
        if entry_is_auto(&entry) {
            let _ = std::fs::remove_file(&entry);
        }
        return false;
    }

    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(_) => return false,
    };
    let desired = desktop_entry(&exe);

    let icon_ok = icons_current();
    let entry_ok = std::fs::read_to_string(&entry)
        .map(|s| s == desired)
        .unwrap_or(false);
    if entry_ok && icon_ok {
        return false;
    }

    let mut wrote = false;
    if let Some(dir) = entry.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if !entry_ok && std::fs::write(&entry, &desired).is_ok() {
        wrote = true;
    }
    if !icon_ok {
        if !icon_is_current() {
            let icon = user_icon();
            if let Some(dir) = icon.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            if std::fs::write(&icon, ICON_SVG).is_ok() {
                wrote = true;
            }
        }
        if write_pngs() {
            wrote = true;
        }
    }
    if wrote {
        refresh_caches();
    }
    wrote
}

/// SVG 与所有位图尺寸都是最新的？
fn icons_current() -> bool {
    icon_is_current()
        && ICON_PNGS.iter().all(|(size, bytes)| {
            std::fs::read(user_png(*size))
                .map(|d| d == *bytes)
                .unwrap_or(false)
        })
}

/// 按内容覆盖位图，返回是否真的写了文件。
fn write_pngs() -> bool {
    let mut wrote = false;
    for (size, bytes) in ICON_PNGS {
        let p = user_png(*size);
        if std::fs::read(&p).map(|d| d == *bytes).unwrap_or(false) {
            continue;
        }
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if std::fs::write(&p, bytes).is_ok() {
            wrote = true;
        }
    }
    wrote
}

fn entry_is_auto(p: &Path) -> bool {
    std::fs::read_to_string(p)
        .map(|s| s.contains(AUTO_MARKER))
        .unwrap_or(false)
}

fn icon_is_current() -> bool {
    std::fs::read_to_string(user_icon())
        .map(|s| s == ICON_SVG)
        .unwrap_or(false)
}

/// 生成自动条目。Exec 用当前二进制的绝对路径，开发运行时也能从 Dock 启动。
fn desktop_entry(exe: &Path) -> String {
    let exe = shell_quote(&exe.display().to_string());
    format!(
        "[Desktop Entry]
Type=Application
Name=OwlMusic
Name[zh_CN]=OwlMusic 音乐
GenericName=Music Player
GenericName[zh_CN]=音乐播放器
Comment=Local music player with lyrics and tag editing
Comment[zh_CN]=本地音乐播放器，支持歌词与标签编辑
Exec={exe} %F
Icon={APP_ID}
Terminal=false
Categories=AudioVideo;Audio;Player;GTK;
Keywords=music;audio;player;lyrics;mp3;flac;
Keywords[zh_CN]=音乐;播放器;歌词;mp3;
StartupNotify=true
StartupWMClass={APP_ID}
MimeType=audio/mpeg;audio/x-mpeg;audio/flac;audio/x-flac;audio/mp4;audio/x-m4a;audio/ogg;audio/x-vorbis+ogg;audio/wav;audio/x-wav;
{AUTO_MARKER}
"
    )
}

/// desktop 条目里的 Exec 按 shell 规则切分，路径有空格时必须用引号包起来。
fn shell_quote(s: &str) -> String {
    if s.chars().any(|c| c.is_whitespace()) {
        format!("\"{}\"", s.replace('"', "\\\""))
    } else {
        s.to_string()
    }
}

fn refresh_caches() {
    let data = data_home();
    for cmd in [
        format!(
            "command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database -q '{}' >/dev/null 2>&1",
            data.join("applications").display()
        ),
        format!(
            "command -v gtk-update-icon-cache >/dev/null 2>&1 && gtk-update-icon-cache -q -t -f '{}' >/dev/null 2>&1",
            data.join("icons/hicolor").display()
        ),
    ] {
        let _ = std::process::Command::new("sh").arg("-c").arg(cmd).status();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_has_required_keys() {
        let s = desktop_entry(Path::new("/opt/Owl Music/owlmusic"));
        assert!(s.starts_with("[Desktop Entry]"));
        assert!(s.contains(&format!("Icon={APP_ID}")));
        assert!(s.contains(&format!("StartupWMClass={APP_ID}")));
        assert!(s.contains(AUTO_MARKER));
        // 带空格的路径要加引号，否则 Exec 会被拆成两个参数
        assert!(s.contains("Exec=\"/opt/Owl Music/owlmusic\" %F"));
    }

    #[test]
    fn plain_path_is_not_quoted() {
        let s = desktop_entry(Path::new("/usr/bin/owlmusic"));
        assert!(s.contains("Exec=/usr/bin/owlmusic %F"));
    }

    #[test]
    fn icon_is_embedded() {
        assert!(ICON_SVG.contains("<svg"));
        assert!(ICON_SVG.contains("</svg>"));
    }

    /// 位图必须真的嵌进来了：缺一张，那个尺寸就会退回旧文件。
    #[test]
    fn all_png_sizes_are_embedded() {
        assert_eq!(ICON_PNGS.len(), 8);
        for (size, bytes) in ICON_PNGS {
            assert!(bytes.len() > 200, "{size}x{size} 的 PNG 太小了");
            assert_eq!(&bytes[1..4], b"PNG", "{size}x{size} 不是 PNG");
        }
    }

    /// 图标不该再画着猫头鹰，主体是一个音符。
    #[test]
    fn icon_drawn_as_music_note() {
        assert!(!ICON_SVG.contains("猫头鹰"));
        assert!(ICON_SVG.contains("八分音符"));
    }
}
