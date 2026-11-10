//! 标签读写的对外门面。
//!
//! 真正的实现在 [`crate::meta`] 的方言层：先判断容器/标签类型，再由对应
//! 方言决定每个字段（曲名、艺术家、专辑、作曲、年份、歌词、封面……）
//! 叫什么记号。这里只是给 UI 用的几个薄封装。

pub use crate::meta::{Cover, MetaPatch, WriteReport};
use std::path::Path;

pub fn read_cover(path: &Path) -> Option<Cover> {
    crate::meta::read_meta_from_path(path).cover
}

pub fn read_lyrics(path: &Path) -> Option<String> {
    crate::meta::read_meta_from_path(path).lyrics
}

/// 只写歌词（等价于一个只含 lyrics 的补丁）。
pub fn write_lyrics(path: &Path, text: &str) -> Result<(), String> {
    crate::meta::write_meta(
        path,
        &MetaPatch {
            lyrics: Some(norm_lf(text)),
            ..Default::default()
        },
    )
    .map(|_| ())
}

/// 写入补丁。返回报告：哪些字段真的写进去了、哪些被该格式跳过了。
pub fn apply(path: &Path, patch: &MetaPatch) -> Result<WriteReport, String> {
    crate::meta::write_meta(path, patch)
}

fn norm_lf(s: &str) -> String {
    s.replace("\r\n", "\n").replace('\r', "\n")
}

/// 把内嵌封面导出成临时文件（给歌词/通知复用时可不用）
#[allow(dead_code)]
pub fn cover_to_temp(cover: &Cover, name: &str) -> Option<std::path::PathBuf> {
    let dir = crate::config::cache_dir().join("covers");
    std::fs::create_dir_all(&dir).ok()?;
    let ext = if cover.mime.contains("png") { "png" } else { "jpg" };
    let safe: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let p = dir.join(format!("{safe}.{ext}"));
    std::fs::write(&p, &cover.data).ok()?;
    Some(p)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// 「选中歌词 → 写进源文件」靠的就是这个：写完必须读得回来。
    /// 拿真实样本跑（没有样本就跳过），MP3 走 ID3v2 的 USLT，FLAC 走 Vorbis 的 LYRICS。
    fn roundtrip(sample: &str, dst_name: &str) {
        let raw = Path::new("target/testmusic").join(sample);
        if !raw.exists() {
            return;
        }
        let dst = std::env::temp_dir().join(format!(
            "owl-lyr-tag-{}-{dst_name}",
            std::process::id()
        ));
        std::fs::copy(&raw, &dst).unwrap();

        let text = "[00:01.00]第一行\n[00:05.00]第二行";
        write_lyrics(&dst, text).expect("写入歌词");
        let got = read_lyrics(&dst).expect("写完应该读得到歌词");
        assert!(got.contains("第一行"), "{sample} 读回：{got}");
        assert!(got.contains("第二行"), "{sample} 读回：{got}");
        std::fs::remove_file(&dst).ok();
    }

    #[test]
    fn lyrics_roundtrip_in_mp3() {
        roundtrip("某人 - MP3 测试.mp3", "a.mp3");
    }

    #[test]
    fn lyrics_roundtrip_in_flac() {
        roundtrip("周杰伦 - 晴天.flac", "a.flac");
    }
}
