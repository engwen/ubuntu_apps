//! 曲库扫描与元数据缓存。扫描结果缓存在 ~/.cache/owlmusic/index.json，
//! 命中 (mtime, size) 就直接复用，避免每次启动都全量读标签。

use lofty::prelude::*;
use lofty::probe::Probe;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

pub const EXTENSIONS: &[&str] = &[
    // 能解码 + 能读标签
    "mp3", "mp2", "mp1", "flac", "m4a", "mp4", "m4b", "aac", "ogg", "oga", "opus", "wav", "aiff",
    "aif", "aifc", "caf", "mka", // 能读标签，但解码器不支持（列在这里是为了还能改标签）
    "wv", "ape", "dsf", "wma",
];

/// 能解码播放的容器/编码（和 Cargo.toml 里 symphonia 的 feature 一一对应，
/// 每一项都用 `owlmusic --probe` 实测过）。
/// 不在这个表里的扩展名仍然会进曲库：标签能读写，只是点播放会明确提示不支持——
/// 总比让用户看着一个点不动的条目、以为程序坏了要好。
const DECODABLE: &[&str] = &[
    "mp3", "flac", "m4a", "mp4", "m4b", "aac", "ogg", "oga", "wav", "aiff", "aif", "aifc", "caf",
    "mka",
];

pub fn extension_of(path: &Path) -> String {
    path.extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

/// symphonia 能不能解码这个文件。注意 .opus 目前不行：
/// symphonia 0.5 只带 vorbis 解码器，ogg 容器里的 opus 音轨会 probe 成功但解不出样本。
pub fn can_decode(path: &Path) -> bool {
    DECODABLE.contains(&extension_of(path).as_str())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Track {
    pub path: PathBuf,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_artist: String,
    /// 作曲（`TCOM` / `COMPOSER` / `©wrt` / `Composer`）
    #[serde(default)]
    pub composer: String,
    pub dur_ms: u64,
    pub track_no: Option<u32>,
    pub disc_no: Option<u32>,
    pub year: Option<u32>,
    pub genre: Option<String>,
    pub has_cover: bool,
    pub has_lyrics: bool,
    pub mtime: u64,
    pub size: u64,
}

impl Track {
    pub fn display_artist(&self) -> &str {
        if self.artist.trim().is_empty() {
            "未知艺人"
        } else {
            &self.artist
        }
    }
    pub fn display_album(&self) -> &str {
        if self.album.trim().is_empty() {
            "未知专辑"
        } else {
            &self.album
        }
    }
    pub fn has_composer(&self) -> bool {
        !self.composer.trim().is_empty() && self.composer.trim() != self.artist.trim()
    }
    /// 侧栏副标题：艺术家 · 专辑（作曲放 tooltip，避免小屏挤爆）
    pub fn display_sub(&self) -> String {
        format!("{} · {}", self.display_artist(), self.display_album())
    }
    #[allow(dead_code)]
    pub fn display_tooltip(&self) -> String {
        let mut s = format!("{} · {}", self.display_artist(), self.display_album());
        if self.has_composer() {
            s.push_str(&format!("\n作曲：{}", self.composer));
        }
        if let Some(y) = self.year {
            s.push_str(&format!("\n年份：{y}"));
        }
        if let Some(g) = &self.genre {
            if !g.trim().is_empty() {
                s.push_str(&format!("\n流派：{g}"));
            }
        }
        s
    }
    /// 文件后缀（小写）。界面上不再展示，保留给检索/调试用。
    #[allow(dead_code)]
    pub fn ext(&self) -> String {
        self.path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase()
    }
}

fn cache_path() -> PathBuf {
    crate::config::cache_dir().join("index.json")
}

pub fn load_cache() -> HashMap<String, Track> {
    let p = cache_path();
    let Ok(s) = std::fs::read_to_string(p) else {
        return HashMap::new();
    };
    let map: HashMap<String, Track> = serde_json::from_str(&s).unwrap_or_default();
    map
}

pub fn save_cache(tracks: &[Track]) {
    let dir = crate::config::cache_dir();
    let _ = std::fs::create_dir_all(&dir);
    let map: HashMap<String, Track> = tracks
        .iter()
        .map(|t| (t.path.to_string_lossy().to_string(), t.clone()))
        .collect();
    if let Ok(s) = serde_json::to_string(&map) {
        let _ = std::fs::write(cache_path(), s);
    }
}

pub fn is_audio(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| {
            let e = e.to_ascii_lowercase();
            EXTENSIONS.contains(&e.as_str())
        })
        .unwrap_or(false)
}

/// 扫描目录（递归）。progress(已处理数, 当前文件)
pub fn scan<F: FnMut(usize, &Path)>(roots: &[PathBuf], mut progress: F) -> Vec<Track> {
    let cache = load_cache();
    let mut out: Vec<Track> = Vec::new();
    let mut seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    let mut n = 0usize;
    for root in roots {
        if root.is_file() {
            if is_audio(root) && seen.insert(root.clone()) {
                n += 1;
                progress(n, root);
                if let Some(t) = track_for(root, &cache) {
                    out.push(t);
                }
            }
            continue;
        }
        let walker = walkdir::WalkDir::new(root)
            .follow_links(false)
            .into_iter()
            .filter_entry(|e| {
                let name = e.file_name().to_string_lossy();
                !(e.file_type().is_dir() && (name.starts_with('.') || name == "node_modules"))
            });
        for entry in walker.flatten() {
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            if !is_audio(path) {
                continue;
            }
            if !seen.insert(path.to_path_buf()) {
                continue;
            }
            n += 1;
            progress(n, path);
            if let Some(t) = track_for(path, &cache) {
                out.push(t);
            }
        }
    }
    out.sort_by(|a, b| {
        a.album
            .to_lowercase()
            .cmp(&b.album.to_lowercase())
            .then(a.disc_no.unwrap_or(0).cmp(&b.disc_no.unwrap_or(0)))
            .then(a.track_no.unwrap_or(0).cmp(&b.track_no.unwrap_or(0)))
            .then(a.path.cmp(&b.path))
    });
    save_cache(&out);
    out
}

fn stat(path: &Path) -> (u64, u64) {
    match std::fs::metadata(path) {
        Ok(m) => {
            let mtime = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            (mtime, m.len())
        }
        Err(_) => (0, 0),
    }
}

fn track_for(path: &Path, cache: &HashMap<String, Track>) -> Option<Track> {
    let (mtime, size) = stat(path);
    let key = path.to_string_lossy().to_string();
    if let Some(t) = cache.get(&key) {
        if t.mtime == mtime && t.size == size {
            return Some(t.clone());
        }
    }
    read_track(path).ok()
}

pub fn read_track(path: &Path) -> Result<Track, String> {
    let (mtime, size) = stat(path);
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("未知曲目")
        .to_string();
    let (mut title, guess_artist) = guess_from_filename(&stem);

    let mut dur_ms = 0u64;
    // 第一步就是「判断这是哪种标签」——后面的每个字段都由该方言读出来，
    // 而不是各字段各自猜一套 ItemKey。
    let meta = match Probe::open(path).and_then(|p| p.read()) {
        Ok(tagged) => {
            dur_ms = tagged.properties().duration().as_millis() as u64;
            crate::meta::read_meta(path, &tagged)
        }
        // 有些文件 lofty 解析不了音频，但标签本身是好的
        // （典型：带 ID3v2.2 标签的 MP3），方言层能直接按帧读
        Err(_) => crate::meta::read_meta_raw(path),
    };

    if let Some(v) = meta.title.filter(|s| !s.trim().is_empty()) {
        title = v;
    }
    let artist = meta
        .artist
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(guess_artist);

    Ok(Track {
        path: path.to_path_buf(),
        title,
        artist,
        album: meta.album.unwrap_or_default(),
        album_artist: meta.album_artist.unwrap_or_default(),
        composer: meta.composer.unwrap_or_default(),
        dur_ms,
        track_no: meta.track,
        disc_no: meta.disc,
        year: meta.year,
        genre: meta.genre,
        has_cover: meta.cover.is_some(),
        has_lyrics: meta.lyrics.as_deref().map(|s| !s.trim().is_empty()).unwrap_or(false),
        mtime,
        size,
    })
}

/// 从文件名猜测 (标题, 艺人)。针对 [社团] 曲名、01. 曲名、艺人 - 曲名 等 ACG 常见命名。
pub fn guess_from_filename(stem: &str) -> (String, String) {
    let mut s = stem.trim().to_string();
    // 去掉开头的音轨号：01 01. 01- 01_
    let digits: String = s.chars().take_while(|c| c.is_ascii_digit()).collect();
    if !digits.is_empty() && digits.len() <= 3 {
        let rest = s[digits.len()..].trim_start_matches(['.', '-', '_', ' ', '、', '　']);
        if !rest.is_empty() && rest != s {
            s = rest.trim().to_string();
        }
    }
    // 去掉常见冗余标记
    for junk in ["[320k]", "(320k)", "[FLAC]", "[flac]", "[MP3]", "[mp3]"] {
        s = s.replace(junk, "");
    }
    s = s.trim().to_string();

    // [社团] 曲名
    if let Some(close) = s.find(']') {
        if s.starts_with('[') {
            let artist = s[1..close].trim().to_string();
            let title = s[close + 1..]
                .trim_start_matches(['-', '_', ' ', '、'])
                .trim()
                .to_string();
            if !title.is_empty() {
                return (title, artist);
            }
        }
    }
    if let Some(close) = s.find('】') {
        if s.starts_with('【') {
            let artist = s['【'.len_utf8()..close].trim().to_string();
            let title = s[close + '】'.len_utf8()..]
                .trim_start_matches(['-', '_', ' ', '、'])
                .trim()
                .to_string();
            if !title.is_empty() {
                return (title, artist);
            }
        }
    }

    // 艺人 - 曲名
    for sep in [" - ", " – ", " — ", "-"] {
        if let Some(i) = s.find(sep) {
            let a = s[..i].trim().to_string();
            let t = s[i + sep.len()..].trim().to_string();
            if !a.is_empty() && !t.is_empty() {
                return (t, a);
            }
        }
    }

    (s, String::new())
}

pub fn fmt_time(ms: u64) -> String {
    let total = ms / 1000;
    let m = total / 60;
    let s = total % 60;
    format!("{m}:{s:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guess_artist_title() {
        let (t, a) = guess_from_filename("周杰伦 - 晴天");
        assert_eq!(t, "晴天");
        assert_eq!(a, "周杰伦");
    }

    #[test]
    fn guess_strips_track_number() {
        let (t, _) = guess_from_filename("01. 夜に駆ける");
        assert_eq!(t, "夜に駆ける");
    }

    #[test]
    fn time_formatting() {
        assert_eq!(fmt_time(0), "0:00");
        assert_eq!(fmt_time(65_000), "1:05");
        assert_eq!(fmt_time(3_600_000), "60:00");
    }
}

#[cfg(test)]
mod codec_tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn decode_whitelist_matches_shipped_features() {
        // 实测能解的
        for ok in ["a.MP3", "b.flac", "c.m4a", "d.ogg", "e.wav", "f.aiff", "g.caf", "h.mka"] {
            assert!(can_decode(&PathBuf::from(ok)), "{ok} 应该能播");
        }
        // opus 缺解码器、ape/wv 没有实现、dsf 是 DSD：能入库但不能播
        for no in ["i.opus", "j.ape", "k.wv", "l.dsf", "m.wma"] {
            assert!(!can_decode(&PathBuf::from(no)), "{no} 不该标记为可播");
        }
        // 没有扩展名不能崩
        assert!(!can_decode(&PathBuf::from("noext")));
    }
}
