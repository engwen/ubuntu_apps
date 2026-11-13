//! APEv2（Monkey's Audio / WavPack / Musepack）。
//!
//! 键名是普通单词：`Title` / `Artist` / `Album` / `Album Artist` /
//! `Composer` / `Genre` / `Year` / `Track` / `Disc` / `Lyrics` / `Comment`。
//! 日期两种写法都有人用：`Year`（四位）和 `Date`（完整日期）。

use super::{Field, TagDialect};
use lofty::tag::{ItemKey, TagType};

pub static DIALECT: Ape = Ape;

pub struct Ape;

impl TagDialect for Ape {
    fn id(&self) -> &'static str {
        "APEv2"
    }

    fn tag_type(&self) -> TagType {
        TagType::Ape
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
        &[("Year", ItemKey::Year), ("Date", ItemKey::RecordingDate)]
    }

    fn markers(&self) -> &'static str {
        "Title / Artist / Album / Album Artist / Composer / Genre / Year / Date / \
         Track / Disc / Lyrics / Comment"
    }
}
