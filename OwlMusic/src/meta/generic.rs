//! 兜底方言：RIFF INFO（WAV）/ AIFF 文本块，以及将来新增的容器。
//!
//! 这些格式的字段名各家不同，但 lofty 已经把它们映射进了统一的
//! `ItemKey`，所以这里按通用键读写；实在没有的字段会进 `skipped`。

use super::{Field, TagDialect};
use lofty::tag::{ItemKey, TagType};

pub static DIALECT: Generic = Generic;

pub struct Generic;

impl TagDialect for Generic {
    fn id(&self) -> &'static str {
        "RIFF/AIFF"
    }

    fn tag_type(&self) -> TagType {
        TagType::RiffInfo
    }

    fn key(&self, field: Field) -> Option<ItemKey> {
        match field {
            Field::Title => Some(ItemKey::TrackTitle),
            Field::Artist => Some(ItemKey::TrackArtist),
            Field::Album => Some(ItemKey::AlbumTitle),
            Field::AlbumArtist => Some(ItemKey::AlbumArtist),
            Field::Composer => Some(ItemKey::Composer),
            Field::Genre => Some(ItemKey::Genre),
            Field::Year => Some(ItemKey::Year),
            Field::Track => Some(ItemKey::TrackNumber),
            Field::Disc => Some(ItemKey::DiscNumber),
            Field::Lyrics => Some(ItemKey::Lyrics),
            Field::Comment => Some(ItemKey::Comment),
            Field::Cover => None,
        }
    }

    fn year_keys(&self) -> &'static [(&'static str, ItemKey)] {
        &[
            ("RecordingDate", ItemKey::RecordingDate),
            ("Year", ItemKey::Year),
        ]
    }

    fn markers(&self) -> &'static str {
        "INAM/IART/IPRD/IGNR/ICRD 等 RIFF INFO 块"
    }
}
