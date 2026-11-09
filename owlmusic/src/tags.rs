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
