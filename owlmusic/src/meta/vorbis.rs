//! VorbisComment（FLAC / Ogg / Opus）。
//!
//! 字段名就是键名：`TITLE` / `ARTIST` / `ALBUM` / `ALBUMARTIST` / `COMPOSER` /
//! `GENRE` / `DATE` / `TRACKNUMBER` / `DISCNUMBER` / `LYRICS` / `COMMENT`。
//! 日期有 `DATE`（规范键，vorbis 1.0）和 `YEAR`（老 FLAC）两种写法，
//! 另外 `ORIGINALDATE` 表示原发行日期。

use super::{Field, TagDialect};
use lofty::tag::{ItemKey, TagType};

pub static DIALECT: Vorbis = Vorbis;

pub struct Vorbis;

impl TagDialect for Vorbis {
    fn id(&self) -> &'static str {
        "VorbisComment"
    }

    fn tag_type(&self) -> TagType {
        TagType::VorbisComments
    }

    fn key(&self, field: Field) -> Option<ItemKey> {
        match field {
            Field::Title => Some(ItemKey::TrackTitle),
            Field::Artist => Some(ItemKey::TrackArtist),
            Field::Album => Some(ItemKey::AlbumTitle),
            Field::AlbumArtist => Some(ItemKey::AlbumArtist),
            Field::Composer => Some(ItemKey::Composer),
            Field::Genre => Some(ItemKey::Genre),
            Field::Year => Some(ItemKey::RecordingDate),
            Field::Track => Some(ItemKey::TrackNumber),
            Field::Disc => Some(ItemKey::DiscNumber),
            Field::Lyrics => Some(ItemKey::Lyrics),
            Field::Comment => Some(ItemKey::Comment),
            Field::Cover => None,
        }
    }

    fn year_keys(&self) -> &'static [(&'static str, ItemKey)] {
        &[
            ("DATE", ItemKey::RecordingDate),
            ("YEAR", ItemKey::Year),
            ("ORIGINALDATE", ItemKey::OriginalReleaseDate),
        ]
    }

    fn markers(&self) -> &'static str {
        "TITLE / ARTIST / ALBUM / ALBUMARTIST / COMPOSER / GENRE / DATE / \
         TRACKNUMBER / DISCNUMBER / LYRICS / COMMENT"
    }
}
