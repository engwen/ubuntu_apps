//! 歌词文件：四级来源的挑选与落盘命名。
//!
//! 查找顺序固定：**音频内嵌标签 → 歌词文件夹 → 音频同目录 → 在线缓存**，
//! 前面的有内容就用前面的。歌词文件夹里认「歌手 - 曲名」「曲名」「01 曲名」
//! 这几种命名，大小写、空格、常见分隔符的差异都按同一套规则归一化后再比，
//! 所以 `周杰伦 - 晴天.lrc` 和 `周杰伦-晴天.LRC` 都能命中。
//!
//! 落盘统一用「歌手 - 曲名.lrc」（没有歌手就只用曲名），同名直接覆盖。

use crate::library::Track;
use std::path::{Path, PathBuf};

/// 歌词从哪儿来的。调用方用它决定状态栏措辞，也用来判断要不要写标签。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum From {
    /// 音频文件内嵌的歌词标签
    Tag,
    /// 设置里指定的歌词文件夹
    Folder,
    /// 音频同目录的 .lrc
    Beside,
    /// 上次在线匹配留下的缓存
    Cache,
}

/// 按优先级挑一份非空歌词。
///
/// 逐个来源判断，空白内容（只有换行 / 空格）视为没有，继续往后找——
/// 用户手滑保存了一个空 .lrc 不该把内嵌歌词挡掉。
pub fn pick(
    tag: Option<String>,
    folder: Option<String>,
    beside: Option<String>,
    cache: Option<String>,
) -> Option<(From, String)> {
    for (from, value) in [
        (From::Tag, tag),
        (From::Folder, folder),
        (From::Beside, beside),
        (From::Cache, cache),
    ] {
        if let Some(s) = value {
            if !s.trim().is_empty() {
                return Some((from, s));
            }
        }
    }
    None
}

/// 在歌词文件夹里找。返回 (命中的文件, 内容)。
///
/// 优先精确同名，其次是归一化后的「歌手 - 曲名 / 曲名 / 序号 曲名」匹配；
/// 多个文件同时命中时取路径字典序最小的，保证每次结果一致。
pub fn find_in_dir(dir: &Path, track: &Track) -> Option<(PathBuf, String)> {
    let entries = std::fs::read_dir(dir).ok()?;

    // 先看有没有和音频同名的（最常见、也最不容易误判）
    if let Some(stem) = track.path.file_stem().and_then(|s| s.to_str()) {
        for entry in std::fs::read_dir(dir).ok()? {
            let Ok(entry) = entry else { continue };
            let p = entry.path();
            if !is_lrc(&p) {
                continue;
            }
            if p.file_stem().and_then(|s| s.to_str()) == Some(stem) {
                if let Some(text) = read_non_empty(&p) {
                    return Some((p, text));
                }
            }
        }
    }

    let cands: Vec<String> = name_candidates(track)
        .iter()
        .map(|s| normalize(s))
        .collect();

    let mut hits: Vec<PathBuf> = Vec::new();
    for entry in entries {
        let Ok(entry) = entry else { continue };
        let p = entry.path();
        if !is_lrc(&p) {
            continue;
        }
        let Some(stem) = p.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let n = normalize(stem);
        if n.is_empty() {
            continue;
        }
        if cands
            .iter()
            .any(|c| !c.is_empty() && (n == *c || strip_index(&n) == c.as_str()))
        {
            hits.push(p);
        }
    }
    hits.sort();
    for p in hits {
        if let Some(text) = read_non_empty(&p) {
            return Some((p, text));
        }
    }
    None
}

/// 音频同目录的 `.lrc`（同名，扩展名大小写不敏感）。
pub fn find_beside(track: &Track) -> Option<(PathBuf, String)> {
    let exact = track.path.with_extension("lrc");
    if let Some(text) = read_non_empty(&exact) {
        return Some((exact, text));
    }
    // Linux 上文件名大小写敏感，但用户从 Windows 拷来的常是 .LRC
    let dir = track.path.parent()?;
    let stem = track.path.file_stem()?.to_str()?.to_string();
    for entry in std::fs::read_dir(dir).ok()? {
        let Ok(entry) = entry else { continue };
        let p = entry.path();
        if !is_lrc(&p) {
            continue;
        }
        if p.file_stem().and_then(|s| s.to_str()) == Some(stem.as_str()) {
            if let Some(text) = read_non_empty(&p) {
                return Some((p, text));
            }
        }
    }
    None
}

/// 把歌词写进歌词文件夹，返回最终文件路径。同名覆盖。
pub fn save(dir: &Path, track: &Track, text: &str) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(file_name(track));
    std::fs::write(&path, text)?;
    Ok(path)
}

/// 落盘用的文件名：「歌手 - 曲名.lrc」。
pub fn file_name(track: &Track) -> String {
    let title = sanitize(track.title.trim());
    let artist = sanitize(track.artist.trim());
    let title = if title.is_empty() {
        // 曲名实在没有就退回音频文件名，总比一个光秃秃的 " - .lrc" 强
        sanitize(
            track
                .path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or(""),
        )
    } else {
        title
    };
    let title = if title.is_empty() {
        "未命名".to_string()
    } else {
        title
    };
    if artist.is_empty() {
        format!("{title}.lrc")
    } else {
        format!("{artist} - {title}.lrc")
    }
}

// ---------------- 内部工具 ----------------

fn is_lrc(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("lrc"))
        .unwrap_or(false)
}

fn read_non_empty(p: &Path) -> Option<String> {
    let text = std::fs::read_to_string(p).ok()?;
    if text.trim().is_empty() {
        None
    } else {
        Some(text)
    }
}

/// 可能的文件名主干，按可信度从高到低。
fn name_candidates(track: &Track) -> Vec<String> {
    let mut v = Vec::new();
    let title = track.title.trim();
    let artist = track.artist.trim();
    if !artist.is_empty() && !title.is_empty() {
        v.push(format!("{artist} - {title}"));
    }
    if !title.is_empty() {
        v.push(title.to_string());
    }
    if let Some(no) = track.track_no {
        if !title.is_empty() {
            v.push(format!("{no:02} {title}"));
            v.push(format!("{no} {title}"));
        }
    }
    if let Some(stem) = track.path.file_stem().and_then(|s| s.to_str()) {
        if !stem.trim().is_empty() {
            v.push(stem.to_string());
        }
    }
    v
}

/// 比较用的归一化：去空白、常见分隔符与标点，统一小写。
///
/// 只用于「这两个名字是不是同一首歌」的判断，不用于显示或落盘。
fn normalize(s: &str) -> String {
    s.chars()
        .filter(|c| !is_noise(*c))
        .flat_map(|c| c.to_lowercase())
        .collect()
}

fn is_noise(c: char) -> bool {
    c.is_whitespace()
        || matches!(
            c,
            '-' | '_'
                | '.'
                | '·'
                | '•'
                | '、'
                | ','
                | '，'
                | '。'
                | '\''
                | '’'
                | '"'
                | '“'
                | '”'
                | '('
                | ')'
                | '（'
                | '）'
                | '['
                | ']'
                | '【'
                | '】'
                | '!'
                | '！'
                | '?'
                | '？'
                | '&'
        )
}

/// 去掉文件名开头的音轨序号：`01晴天` → `晴天`。
fn strip_index(s: &str) -> &str {
    let rest = s.trim_start_matches(|c: char| c.is_ascii_digit() || c == '#');
    if rest.len() < s.len() {
        // 序号后面可能还跟着被归一到一起的分隔符，这里再削一层
        rest.trim_start_matches(is_noise)
    } else {
        s
    }
}

/// 去掉路径里不能出现的字符，并收掉多余空白。
fn sanitize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c == '\0' || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
            // 用全角字符代替，既保留可读性又不会让文件系统报错
            out.push(match c {
                '/' | '\\' => '／',
                ':' => '：',
                '*' => '＊',
                '?' => '？',
                '"' => '＂',
                '<' => '＜',
                '>' => '＞',
                '|' => '｜',
                _ => ' ',
            });
        } else if c.is_control() {
            out.push(' ');
        } else {
            out.push(c);
        }
    }
    let s = out.trim().trim_end_matches('.').trim().to_string();
    // 文件名过长会让部分文件系统直接失败，按字符截断而不是字节
    s.chars().take(120).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn track(dir: &Path, stem: &str, artist: &str, title: &str) -> Track {
        Track {
            path: dir.join(format!("{stem}.flac")),
            title: title.into(),
            artist: artist.into(),
            album: "专辑".into(),
            album_artist: String::new(),
            composer: String::new(),
            dur_ms: 200_000,
            track_no: None,
            disc_no: None,
            year: None,
            genre: None,
            has_cover: false,
            has_lyrics: false,
            mtime: 0,
            size: 0,
        }
    }

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("owlmusic-lyrics-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn pick_follows_priority_and_skips_blank() {
        assert_eq!(
            pick(Some("  \n".into()), Some("folder".into()), None, None),
            Some((From::Folder, "folder".into()))
        );
        assert_eq!(
            pick(Some("tag".into()), Some("folder".into()), None, None),
            Some((From::Tag, "tag".into()))
        );
        assert_eq!(pick(None, None, None, None), None);
    }

    #[test]
    fn finds_artist_title_variants() {
        let d = tmp("variants");
        std::fs::write(d.join("周杰伦-晴天.LRC"), "[00:01]故事的小黄花").unwrap();
        let t = track(&d, "晴天", "周杰伦", "晴天");
        let (p, text) = find_in_dir(&d, &t).expect("应命中");
        assert_eq!(p.file_name().unwrap(), "周杰伦-晴天.LRC");
        assert!(text.contains("小黄花"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn finds_title_only_and_index_prefix() {
        let d = tmp("index");
        std::fs::write(d.join("03 晴天.lrc"), "歌词").unwrap();
        let t = track(&d, "track03", "周杰伦", "晴天");
        assert!(find_in_dir(&d, &t).is_some());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn ignores_other_songs_and_empty_files() {
        let d = tmp("other");
        std::fs::write(d.join("晴天.lrc"), "   \n").unwrap();
        std::fs::write(d.join("七里香.lrc"), "歌词").unwrap();
        let t = track(&d, "晴天", "周杰伦", "晴天");
        assert!(find_in_dir(&d, &t).is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn find_beside_matches_case_insensitive_extension() {
        let d = tmp("beside");
        std::fs::write(d.join("Song.LRC"), "abc").unwrap();
        let t = track(&d, "Song", "A", "Song");
        assert!(find_beside(&t).is_some());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn save_uses_artist_and_overwrites() {
        let d = tmp("save");
        let t = track(&d, "晴天", "周杰伦", "晴天");
        let p = save(&d, &t, "第一版").unwrap();
        assert_eq!(p.file_name().unwrap(), "周杰伦 - 晴天.lrc");
        save(&d, &t, "第二版").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "第二版");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn file_name_sanitizes_slashes() {
        let d = tmp("sanitize");
        let t = track(&d, "x", "AC/DC", "A:B");
        let n = file_name(&t);
        assert!(!n.contains('/'));
        assert!(!n.contains(':'));
        assert!(n.ends_with(".lrc"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn find_beside_prefers_exact_name() {
        let d = tmp("exact");
        std::fs::write(d.join("Song.lrc"), "right").unwrap();
        std::fs::write(d.join("song.lrc"), "wrong").unwrap();
        let t = track(&d, "Song", "A", "Song");
        assert_eq!(find_beside(&t).unwrap().1, "right");
        let _ = std::fs::remove_dir_all(&d);
    }
}
