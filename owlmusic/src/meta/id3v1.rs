//! ID3v1 / ID3v1.1：文件尾 128 字节，字段全部定长。
//!
//! | 字段   | 位置    | 长度 |
//! |--------|---------|------|
//! | 标题   | 3..33   | 30   |
//! | 艺术家 | 33..63  | 30   |
//! | 专辑   | 63..93  | 30   |
//! | 年份   | 93..97  | 4    |
//! | 注释   | 97..127 | 30   |
//! | 流派   | 127     | 1    |
//!
//! 编码是 Latin-1，**中文写进去必然是乱码**，所以非 ASCII 字段这里会
//! 明确跳过并在报告里说明，而不是硬写一堆问号。

use super::{Field, MetaPatch, TagDialect, WriteReport};
use lofty::tag::{ItemKey, TagType};
use std::path::Path;

pub static DIALECT: Id3v1 = Id3v1;

pub struct Id3v1;

impl TagDialect for Id3v1 {
    fn id(&self) -> &'static str {
        "ID3v1"
    }

    fn tag_type(&self) -> TagType {
        TagType::Id3v1
    }

    fn key(&self, field: Field) -> Option<ItemKey> {
        match field {
            Field::Title => Some(ItemKey::TrackTitle),
            Field::Artist => Some(ItemKey::TrackArtist),
            Field::Album => Some(ItemKey::AlbumTitle),
            Field::Year => Some(ItemKey::Year),
            Field::Comment => Some(ItemKey::Comment),
            Field::Track => Some(ItemKey::TrackNumber),
            _ => None,
        }
    }

    fn year_keys(&self) -> &'static [(&'static str, ItemKey)] {
        &[("year", ItemKey::Year)]
    }

    fn markers(&self) -> &'static str {
        "title(30) / artist(30) / album(30) / year(4) / comment(30)，Latin-1"
    }

    fn supports_cover(&self) -> bool {
        false
    }

    fn write(&self, path: &Path, patch: &MetaPatch) -> Result<WriteReport, String> {
        let mut safe = MetaPatch::default();
        let mut skipped: Vec<(&'static str, String)> = Vec::new();

        for f in [
            Field::Title,
            Field::Artist,
            Field::Album,
            Field::Comment,
        ] {
            let Some(v) = patch.value(f) else { continue };
            if v.chars().all(|c| c as u32 <= 0xFF) && !v.trim().is_empty() {
                if v.len() > 30 {
                    skipped.push((
                        f.name(),
                        format!("ID3v1 单字段最长 30 字节（现在 {}），已跳过", v.len()),
                    ));
                    continue;
                }
                match f {
                    Field::Title => safe.title = Some(v),
                    Field::Artist => safe.artist = Some(v),
                    Field::Album => safe.album = Some(v),
                    Field::Comment => safe.comment = Some(v),
                    _ => {}
                }
            } else if v.trim().is_empty() {
                match f {
                    Field::Title => safe.title = Some(String::new()),
                    Field::Artist => safe.artist = Some(String::new()),
                    Field::Album => safe.album = Some(String::new()),
                    Field::Comment => safe.comment = Some(String::new()),
                    _ => {}
                }
            } else {
                skipped.push((
                    f.name(),
                    "ID3v1 只支持 Latin-1，中文会被写成乱码，已跳过".into(),
                ));
            }
        }
        safe.year = patch.year;
        safe.track = patch.track;

        // 明确不支持的字段
        for f in [
            Field::AlbumArtist,
            Field::Composer,
            Field::Genre,
            Field::Disc,
            Field::Lyrics,
        ] {
            if patch.value(f).is_some() {
                skipped.push((f.name(), "ID3v1 没有该字段".into()));
            }
        }
        if patch.cover.is_some() {
            skipped.push((Field::Cover.name(), "ID3v1 不能存图片".into()));
        }

        let mut report = super::default_write(path, &safe, self)?;
        report.skipped.extend(skipped);
        Ok(report)
    }
}
