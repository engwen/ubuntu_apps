//! MP4 / M4A（`ilst` 原子）。
//!
//! | 字段       | 原子     |
//! |------------|----------|
//! | 标题       | `©nam`   |
//! | 艺术家     | `©ART`   |
//! | 专辑       | `©alb`   |
//! | 专辑艺术家 | `aART`   |
//! | 作曲       | `©wrt`   |
//! | 流派       | `©gen`   |
//! | 年份       | `©day`   |
//! | 音轨       | `trkn`   |
//! | 碟号       | `disk`   |
//! | 歌词       | `©lyr`   |
//! | 注释       | `©cmt`   |

use super::{Field, TagDialect};
use lofty::tag::{ItemKey, TagType};

pub static DIALECT: Mp4 = Mp4;

pub struct Mp4;

impl TagDialect for Mp4 {
    fn id(&self) -> &'static str {
        "MP4/ilst"
    }

    fn tag_type(&self) -> TagType {
        TagType::Mp4Ilst
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
        &[("©day", ItemKey::RecordingDate)]
    }

    fn markers(&self) -> &'static str {
        "©nam / ©ART / ©alb / aART / ©wrt / ©gen / ©day / trkn / disk / ©lyr / ©cmt"
    }
}
