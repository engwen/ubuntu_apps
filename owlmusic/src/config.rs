//! 配置持久化：~/.config/owlmusic/config.json

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct LyricStyle {
    pub font_size: i32,
    /// "accent" 表示跟随主题/封面主色
    pub color: String,
    /// 0 左 1 中 2 右
    pub align: u8,
    /// 歌词整体垂直位置微调（像素）
    pub offset_px: i32,
    pub line_spacing: i32,
    /// 非当前行不透明度
    pub dim: f32,
    pub show_trans: bool,
    pub hide_empty: bool,
}

impl Default for LyricStyle {
    fn default() -> Self {
        LyricStyle {
            font_size: 22,
            color: "text".into(),
            align: 1,
            offset_px: 0,
            line_spacing: 14,
            dim: 0.34,
            show_trans: true,
            hide_empty: true,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub roots: Vec<PathBuf>,
    pub volume: f32,
    /// dark | light
    pub theme: String,
    pub accent: String,
    pub accent_follow_cover: bool,
    pub shuffle: bool,
    /// 0 关 1 列表循环 2 单曲循环
    pub repeat: u8,
    /// 歌单显示在窗口右侧
    pub sidebar_right: bool,
    /// 歌词页使用专辑封面做背景
    pub cover_bg: bool,
    /// 封面背景模糊强度 0.2–2.0
    pub blur: f32,
    /// 歌词区背景（模糊封面）的不透明度 0.05–0.95
    pub lyric_bg_opacity: f32,
    /// 播放时若无歌词自动在线匹配
    pub auto_lyric: bool,
    /// 启动时接着上次的位置
    pub resume: bool,
    /// 上次播放进度（毫秒）
    pub last_pos_ms: u64,
    /// 在线搜索区域：cn / jp / us / tw
    pub country: String,
    pub lyric: LyricStyle,
    pub last_track: Option<PathBuf>,
    pub win_w: i32,
    pub win_h: i32,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            roots: Vec::new(),
            volume: 0.85,
            theme: "dark".into(),
            accent: "#8B7CF6".into(),
            accent_follow_cover: true,
            shuffle: false,
            repeat: 1,
            sidebar_right: false,
            cover_bg: true,
            blur: 1.0,
            lyric_bg_opacity: 0.5,
            auto_lyric: false,
            resume: true,
            last_pos_ms: 0,
            country: "jp".into(),
            lyric: LyricStyle::default(),
            last_track: None,
            win_w: 1200,
            win_h: 780,
        }
    }
}

pub fn config_dir() -> PathBuf {
    let base = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into())).join(".config"));
    base.join("owlmusic")
}

pub fn cache_dir() -> PathBuf {
    let base = std::env::var("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into())).join(".cache"));
    base.join("owlmusic")
}

fn config_path() -> PathBuf {
    config_dir().join("config.json")
}

impl Config {
    pub fn load() -> Self {
        let p = config_path();
        match std::fs::read_to_string(&p) {
            Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
            Err(_) => Config::default(),
        }
    }

    pub fn save(&self) {
        let dir = config_dir();
        let _ = std::fs::create_dir_all(&dir);
        if let Ok(s) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(config_path(), s);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_keeps_values() {
        let c = Config::default();
        let s = serde_json::to_string(&c).unwrap();
        let d: Config = serde_json::from_str(&s).unwrap();
        assert_eq!(d.volume, c.volume);
        assert_eq!(d.blur, c.blur);
        assert_eq!(d.lyric_bg_opacity, c.lyric_bg_opacity);
        assert_eq!(d.lyric.font_size, c.lyric.font_size);
        assert!(d.resume);
    }

    #[test]
    fn missing_fields_fall_back_to_default() {
        let d: Config = serde_json::from_str(r#"{"volume":0.42}"#).unwrap();
        assert!((d.volume - 0.42).abs() < 1e-6);
        assert_eq!(d.win_w, Config::default().win_w);
        assert_eq!(d.lyric.dim, Config::default().lyric.dim);
    }
}
