//! 标签方言适配层。
//!
//! 一个语义在不同容器里的记号是完全不同的。以「年份」为例：ID3v2.4 是
//! `TDRC`，ID3v2.3 是 `TYER`，ID3v2.2 是 `TYE`，Vorbis 是 `DATE`/`YEAR`，
//! MP4 是 `©day`，APE 是 `Year`，ID3v1 只是末尾四个字节；作曲同理
//! （`TCOM` / `COMPOSER` / `©wrt` / `Composer`）。
//!
//! 所以这里不针对某个字段做特判，而是**先判断标签类型，再把整张表的读写
//! 都交给该类型的方言实现**：
//!
//! ```text
//! Track/文件
//!   └─ 探测容器与标签版本（ID3v2.2/2.3/2.4、ID3v1、VorbisComment、ilst、APEv2…）
//!        └─ dialect_for(tag_type)
//!             ├─ read(tag)     通用抽象能看到的字段
//!             ├─ read_raw(path) 通用抽象看不到的记号（TXXX:YEAR、v2.2 帧…）
//!             └─ write(path)   按该方言写；ID3 家族总是整体重建为 v2.4
//! ```
//!
//! 读取结果统一成 [`TrackMeta`]，写入统一接受 [`MetaPatch`] 并返回
//! [`WriteReport`]（哪些字段真的写进去了、哪些被这个格式跳过了）。

use lofty::file::TaggedFile;
use lofty::prelude::*;
use lofty::tag::{ItemKey, Tag, TagType};
use std::path::Path;

pub mod ape;
pub mod generic;
pub mod id3v1;
pub mod id3v2;
pub mod mp4;
pub mod vorbis;

/// 一个可读写的语义字段。所有方言共用这套词汇。
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Field {
    Title,
    Artist,
    Album,
    AlbumArtist,
    Composer,
    Genre,
    Year,
    Track,
    Disc,
    Lyrics,
    Comment,
    Cover,
}

impl Field {
    pub const TEXT_FIELDS: [Field; 11] = [
        Field::Title,
        Field::Artist,
        Field::Album,
        Field::AlbumArtist,
        Field::Composer,
        Field::Genre,
        Field::Year,
        Field::Track,
        Field::Disc,
        Field::Lyrics,
        Field::Comment,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Field::Title => "曲名",
            Field::Artist => "艺术家",
            Field::Album => "专辑",
            Field::AlbumArtist => "专辑艺术家",
            Field::Composer => "作曲",
            Field::Genre => "流派",
            Field::Year => "年份",
            Field::Track => "音轨号",
            Field::Disc => "碟号",
            Field::Lyrics => "歌词",
            Field::Comment => "注释",
            Field::Cover => "封面",
        }
    }
}

/// 内嵌图片。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cover {
    pub data: Vec<u8>,
    pub mime: String,
}

impl Cover {
    pub fn mime_guess(data: &[u8]) -> &'static str {
        if data.starts_with(&[0xFF, 0xD8]) {
            "image/jpeg"
        } else if data.starts_with(&[0x89, b'P', b'N', b'G']) {
            "image/png"
        } else if data.starts_with(b"GIF8") {
            "image/gif"
        } else if data.len() > 12 && &data[..4] == b"RIFF" && &data[8..12] == b"WEBP" {
            "image/webp"
        } else {
            "image/jpeg"
        }
    }
}

/// 从文件里读出来的全部元数据（各字段独立，缺就是 `None`）。
#[derive(Clone, Debug, Default)]
pub struct TrackMeta {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub composer: Option<String>,
    pub genre: Option<String>,
    pub year: Option<u32>,
    /// 年份实际命中的记号（`TDRC` / `TXXX:YEAR` / `DATE` / `©day`…），排查用
    pub year_marker: Option<String>,
    pub track: Option<u32>,
    pub track_total: Option<u32>,
    pub disc: Option<u32>,
    pub disc_total: Option<u32>,
    pub lyrics: Option<String>,
    pub comment: Option<String>,
    pub cover: Option<Cover>,
    /// 读它的方言名
    pub dialect: Option<&'static str>,
    /// 标签版本说明，如 `ID3v2.3`、`VorbisComment`、`ID3v1`
    pub version: Option<String>,
}

impl TrackMeta {
    /// 只补空缺，不覆盖已有值。用于「主标签优先、老标签兜底」的合并。
    pub fn absorb(&mut self, other: TrackMeta) {
        fn take(dst: &mut Option<String>, src: Option<String>) {
            if dst.as_ref().map(|s| s.trim().is_empty()).unwrap_or(true) {
                if let Some(v) = src.filter(|s| !s.trim().is_empty()) {
                    *dst = Some(v);
                }
            }
        }
        take(&mut self.title, other.title);
        take(&mut self.artist, other.artist);
        take(&mut self.album, other.album);
        take(&mut self.album_artist, other.album_artist);
        take(&mut self.composer, other.composer);
        take(&mut self.genre, other.genre);
        take(&mut self.lyrics, other.lyrics);
        take(&mut self.comment, other.comment);
        if self.year.is_none() && other.year.is_some() {
            self.year = other.year;
            self.year_marker = other.year_marker;
        }
        if self.track.is_none() {
            self.track = other.track;
        }
        if self.track_total.is_none() {
            self.track_total = other.track_total;
        }
        if self.disc.is_none() {
            self.disc = other.disc;
        }
        if self.disc_total.is_none() {
            self.disc_total = other.disc_total;
        }
        if self.cover.is_none() {
            self.cover = other.cover;
        }
        if self.dialect.is_none() {
            self.dialect = other.dialect;
            self.version = other.version;
        }
    }

    /// 有没有值得一提的内容（用来判断某个标签是不是空的）
    pub fn is_empty(&self) -> bool {
        self.title.is_none()
            && self.artist.is_none()
            && self.album.is_none()
            && self.composer.is_none()
            && self.year.is_none()
            && self.lyrics.is_none()
            && self.cover.is_none()
    }
}

/// 写入结果：真的写了什么、这个格式不支持什么。
#[derive(Clone, Debug, Default)]
pub struct WriteReport {
    pub dialect: String,
    pub written: Vec<&'static str>,
    /// 字段 + 原因（例如「ID3v1 不支持该字段」「已截断到 30 字节」）
    pub skipped: Vec<(&'static str, String)>,
    /// 过程说明：版本升级、丢弃的旧帧、移除的副标签……
    pub notes: Vec<String>,
}

impl WriteReport {
    pub fn new(dialect: &str) -> Self {
        Self {
            dialect: dialect.to_string(),
            ..Default::default()
        }
    }

    pub fn summary(&self) -> String {
        let mut s = format!("已写入 {} 个字段（{}）", self.written.len(), self.dialect);
        if !self.written.is_empty() {
            s.push_str("：");
            s.push_str(&self.written.join("、"));
        }
        if !self.skipped.is_empty() {
            s.push_str("；跳过 ");
            s.push_str(
                &self
                    .skipped
                    .iter()
                    .map(|(f, why)| format!("{f}（{why}）"))
                    .collect::<Vec<_>>()
                    .join("、"),
            );
        }
        if !self.notes.is_empty() {
            s.push_str("；");
            s.push_str(&self.notes.join("；"));
        }
        s
    }
}

/// 一种标签方言。
///
/// 实现者只需要给出「这个字段在本格式里叫什么」，读取与写入的骨架都由
/// 默认实现完成；像 ID3v2 这种有私有帧、需要保真改写的方言再覆盖
/// [`TagDialect::read_raw`] / [`TagDialect::write`]。
pub trait TagDialect: Send + Sync {
    /// 方言名，用于提示
    fn id(&self) -> &'static str;

    /// 该方言对应的 lofty 标签类型（新建标签时用）
    fn tag_type(&self) -> TagType;

    /// 字段 → 记号。`None` 表示这个格式根本没有这个字段。
    fn key(&self, field: Field) -> Option<ItemKey>;

    /// 年份在本格式里的记号，按优先级排列（第一个是写入时用的）。
    /// 单独列出来是因为同一个语义常有多个历史写法。
    fn year_keys(&self) -> &'static [(&'static str, ItemKey)] {
        &[
            ("RecordingDate", ItemKey::RecordingDate),
            ("Year", ItemKey::Year),
        ]
    }

    /// 本格式的记号一览（错误提示用）
    fn markers(&self) -> &'static str;

    /// 从通用标签里读所有字段
    fn read(&self, tag: &Tag) -> TrackMeta {
        let mut m = TrackMeta {
            dialect: Some(self.id()),
            version: Some(self.id().to_string()),
            ..Default::default()
        };
        m.title = self.text_at(tag, Field::Title);
        m.artist = self.text_at(tag, Field::Artist);
        m.album = self.text_at(tag, Field::Album);
        m.album_artist = self.text_at(tag, Field::AlbumArtist);
        m.composer = self.text_at(tag, Field::Composer);
        m.genre = self.text_at(tag, Field::Genre);
        m.lyrics = self.text_at(tag, Field::Lyrics);
        m.comment = self.text_at(tag, Field::Comment);
        if let Some(v) = self.text_at(tag, Field::Track) {
            let (n, total) = parse_num_pair(&v);
            m.track = n;
            m.track_total = total;
        }
        if let Some(v) = self.text_at(tag, Field::Disc) {
            let (n, total) = parse_num_pair(&v);
            m.disc = n;
            m.disc_total = total;
        }
        for (marker, key) in self.year_keys() {
            if let Some(raw) = tag.get_string(*key) {
                if let Some(y) = parse_year(raw) {
                    m.year = Some(y);
                    m.year_marker = Some((*marker).to_string());
                    break;
                }
            }
        }
        m.cover = tag.pictures().first().map(|p| Cover {
            data: p.data().to_vec(),
            mime: p
                .mime_type()
                .map(|m| m.as_str().to_string())
                .unwrap_or_else(|| Cover::mime_guess(p.data()).to_string()),
        });
        m
    }

    /// 通用抽象看不到的记号，回原始字节里找。默认没有。
    fn read_raw(&self, _path: &Path) -> TrackMeta {
        TrackMeta::default()
    }

    /// 写入。默认走 lofty 通用写入（保留该格式的其余字段）。
    fn write(&self, path: &Path, patch: &MetaPatch) -> Result<WriteReport, String> {
        default_write(path, patch, self)
    }

    /// 这个格式能不能存内嵌图片
    fn supports_cover(&self) -> bool {
        true
    }

    /// 取某字段的文本（内部小工具）
    fn text_at(&self, tag: &Tag, field: Field) -> Option<String> {
        let k = self.key(field)?;
        tag.get_string(k)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }
}

/// 按标签类型取方言实现。**这是所有读写的唯一入口分派点。**
pub fn dialect_for(tag_type: TagType) -> &'static dyn TagDialect {
    match tag_type {
        TagType::Id3v2 => &id3v2::DIALECT,
        TagType::Id3v1 => &id3v1::DIALECT,
        TagType::VorbisComments => &vorbis::DIALECT,
        TagType::Mp4Ilst => &mp4::DIALECT,
        TagType::Ape => &ape::DIALECT,
        TagType::RiffInfo | TagType::AiffText => &generic::DIALECT,
        _ => &generic::DIALECT,
    }
}

/// 给还没有任何标签的文件猜一个方言（按扩展名）。
pub fn dialect_for_path(path: &Path) -> &'static dyn TagDialect {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "flac" | "ogg" | "opus" => &vorbis::DIALECT,
        "m4a" | "mp4" | "aac" | "m4b" => &mp4::DIALECT,
        "ape" | "wv" | "mpc" => &ape::DIALECT,
        "wav" | "aiff" | "aif" => &generic::DIALECT,
        _ => &id3v2::DIALECT,
    }
}

/// 一个标签在合并时的优先级：信息量大的优先。
fn rank(tt: TagType) -> u8 {
    match tt {
        TagType::Id3v2 | TagType::VorbisComments | TagType::Mp4Ilst => 0,
        TagType::Ape => 1,
        TagType::RiffInfo | TagType::AiffText => 2,
        TagType::Id3v1 => 3,
        _ => 2,
    }
}

/// 从已解析的文件里读出全部字段。
///
/// 顺序：按标签优先级遍历（ID3v2/Vorbis/ilst 优先，ID3v1 最后兜底），
/// 每个标签先走通用抽象，再让方言回原始字节补空缺。
pub fn read_meta(path: &Path, tagged: &TaggedFile) -> TrackMeta {
    let mut tags: Vec<&Tag> = tagged.tags().iter().collect();
    tags.sort_by_key(|t| rank(t.tag_type()));

    let mut out = TrackMeta::default();
    let mut id3v2_raw_done = false;
    for tag in tags {
        // ID3v2 自己按帧读最准（版本号、TXXX 私货、v2.2 都能拿到），
        // 读不到再退回 lofty 的通用抽象。
        if !id3v2_raw_done && tag.tag_type() == TagType::Id3v2 {
            id3v2_raw_done = true;
            let raw = id3v2::read_all_raw(path);
            if !raw.is_empty() {
                out.absorb(raw);
                continue;
            }
        }
        let d = dialect_for(tag.tag_type());
        out.absorb(d.read(tag));
    }

    // 一个标签都没有：只能按扩展名猜方言，回原始字节读
    if out.dialect.is_none() {
        out.absorb(read_meta_raw(path));
    }
    finalize(out)
}

/// 收尾：有些软件把 LRC 塞在注释里，注释像时间轴歌词就当归歌词。
fn finalize(mut m: TrackMeta) -> TrackMeta {
    if m.lyrics.is_none() {
        if let Some(c) = &m.comment {
            if crate::lrc::looks_synced(c) {
                m.lyrics = Some(c.clone());
            }
        }
    }
    m
}

/// 完全绕开 lofty 的读取（ID3v2.2 会让 lofty 的 MPEG 解析器直接报错）。
pub fn read_meta_raw(path: &Path) -> TrackMeta {
    let mut out = TrackMeta::default();
    // ID3 头是最可靠的信号，先试
    if let Some(m) = id3v2::read_all_raw_opt(path) {
        out.absorb(m);
    }
    if out.is_empty() {
        let d = dialect_for_path(path);
        out.absorb(d.read_raw(path));
    }
    finalize(out)
}

/// 不依赖调用方已经解析过文件，直接从磁盘读。
pub fn read_meta_from_path(path: &Path) -> TrackMeta {
    match lofty::probe::Probe::open(path).and_then(|p| p.read()) {
        Ok(tagged) => {
            let m = read_meta(path, &tagged);
            if m.is_empty() {
                read_meta_raw(path)
            } else {
                m
            }
        }
        Err(_) => read_meta_raw(path),
    }
}

/// 写入元数据：**所有字段修改的唯一公共入口**。
///
/// 调用方不需要知道自己改的是哪个字段、文件原来是什么版本——这里先判断
/// 容器，再交给对应方言。ID3 家族（mp3/mp2/aac 及带 ID3 头或尾的文件）
/// 一律走 [`id3v2::write_v24`]，把整张标签重建为 v2.4；其余容器用它自己的
/// 标准标签（FLAC/OGG → VorbisComment，M4A → ilst，APE → APEv2），因为这些
/// 格式根本不能存 ID3v2.4。
///
/// 写完会读回校验——`insert_text` 返回 true 只代表标签对象收下了，
/// 不代表容器真能存下这个记号。
pub fn write_meta(path: &Path, patch: &MetaPatch) -> Result<WriteReport, String> {
    if patch.is_empty() {
        return Ok(WriteReport::new("无改动"));
    }
    if let Some(y) = patch.year {
        if !(1000..=2999).contains(&y) {
            return Err(format!("年份超出合理范围: {y}"));
        }
    }
    let dialect = detect_write_dialect(path);
    let report = dialect.write(path, patch)?;
    verify(path, patch, &report)?;
    Ok(report)
}

/// 这个文件是不是「ID3 家族」——标签只能/应该用 ID3v2 的容器。
///
/// 判断顺序：先看扩展名（`.m4a` 这种必须是 ilst，不能误判），
/// 再看文件头/尾有没有 ID3 标签。
pub fn is_id3_container(path: &Path) -> bool {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        // 这些容器的标签格式是写死的，不可能换成 ID3v2.4
        "flac" | "ogg" | "oga" | "opus" | "m4a" | "mp4" | "m4b" | "ape" | "wv" | "mpc" | "wav"
        | "aiff" | "aif" => false,
        // MPEG 音频：标签本来就是 ID3
        "mp3" | "mp2" | "mp1" | "mpga" | "aac" => true,
        _ => id3v2::has_id3v2_header(path) || id3v2::has_id3v1_tail(path),
    }
}

/// 要往这个文件里写，用哪种方言？
///
/// ID3 家族一律 ID3v2（写入时整体升级到 v2.4）；其余看文件里**已经存在**
/// 的标签，没有标签才按扩展名决定新建哪种。
fn detect_write_dialect(path: &Path) -> &'static dyn TagDialect {
    if is_id3_container(path) {
        return &id3v2::DIALECT;
    }
    if let Ok(probe) = lofty::probe::Probe::open(path) {
        if let Ok(tagged) = probe.read() {
            let mut tags: Vec<&Tag> = tagged.tags().iter().collect();
            tags.sort_by_key(|t| rank(t.tag_type()));
            if let Some(tag) = tags.first() {
                return dialect_for(tag.tag_type());
            }
        }
    }
    dialect_for_path(path)
}

fn verify(path: &Path, patch: &MetaPatch, report: &WriteReport) -> Result<(), String> {
    let got = read_meta_from_path(path);
    let mut problems = Vec::new();
    let skipped: Vec<&str> = report.skipped.iter().map(|(f, _)| *f).collect();

    let mut check_text = |field: Field, want: &Option<String>, got: &Option<String>| {
        if skipped.contains(&field.name()) {
            return;
        }
        let want = match want {
            Some(v) if !v.trim().is_empty() => v.trim(),
            _ => return,
        };
        let got = got.as_deref().unwrap_or("").trim();
        if got != want {
            problems.push(format!(
                "{}：期望「{want}」，读回「{got}」",
                field.name()
            ));
        }
    };

    check_text(Field::Title, &patch.title, &got.title);
    check_text(Field::Artist, &patch.artist, &got.artist);
    check_text(Field::Album, &patch.album, &got.album);
    check_text(Field::AlbumArtist, &patch.album_artist, &got.album_artist);
    check_text(Field::Composer, &patch.composer, &got.composer);
    check_text(Field::Genre, &patch.genre, &got.genre);
    check_text(Field::Lyrics, &patch.lyrics, &got.lyrics);
    check_text(Field::Comment, &patch.comment, &got.comment);

    if let Some(want) = patch.year {
        if got.year != Some(want) {
            problems.push(format!(
                "年份：期望 {want}，读回 {}（来自 {}）",
                got.year
                    .map(|y| y.to_string())
                    .unwrap_or_else(|| "无".into()),
                got.year_marker.as_deref().unwrap_or("无")
            ));
        }
    }
    if let Some(want) = patch.track {
        if got.track != Some(want) {
            problems.push(format!("音轨号：期望 {want}，读回 {:?}", got.track));
        }
    }
    if let Some(want) = patch.disc {
        if got.disc != Some(want) {
            problems.push(format!("碟号：期望 {want}，读回 {:?}", got.disc));
        }
    }
    if patch.remove_cover {
        if got.cover.is_some() {
            problems.push("封面：删除后仍然读得到".to_string());
        }
    } else if let Some(want) = &patch.cover {
        match &got.cover {
            Some(c) if c.data == want.data => {}
            Some(_) => problems.push("封面：读回的图片与写入的不一致".to_string()),
            None => problems.push("封面：写入后读不到封面".to_string()),
        }
    }

    if problems.is_empty() {
        Ok(())
    } else {
        let hints = detect_write_dialect(path).markers();
        Err(format!(
            "写入校验失败：{}\n该格式的记号：{hints}",
            problems.join("；")
        ))
    }
}

/// 通用的 lofty 写入路径：按方言的 key 映射逐字段写。
pub fn default_write<D: TagDialect + ?Sized>(
    path: &Path,
    patch: &MetaPatch,
    d: &D,
) -> Result<WriteReport, String> {
    let mut tagged = lofty::probe::Probe::open(path)
        .map_err(|e| format!("打开失败: {e}"))?
        .read()
        .map_err(|e| format!("解析失败: {e}"))?;
    let tt = d.tag_type();
    if tagged.tag(tt).is_none() {
        tagged.insert_tag(Tag::new(tt));
    }
    // ID3v1 不是 primary tag，必须按类型取，不能用 primary_tag_mut
    let tag = if tagged.tag(tt).is_some() {
        tagged.tag_mut(tt).ok_or("无法创建标签")?
    } else {
        tagged.primary_tag_mut().ok_or("无法创建标签")?
    };

    let mut report = WriteReport::new(d.id());
    for f in Field::TEXT_FIELDS {
        let Some(v) = patch.value(f) else { continue };
        match d.key(f) {
            Some(k) => {
                if v.trim().is_empty() {
                    tag.remove_key(k);
                } else {
                    tag.insert_text(k, v);
                }
                report.written.push(f.name());
            }
            None => report
                .skipped
                .push((f.name(), format!("{} 没有对应字段", d.id()))),
        }
    }

    if patch.remove_cover || patch.cover.is_some() {
        if !d.supports_cover() {
            report
                .skipped
                .push((Field::Cover.name(), format!("{} 不支持内嵌图片", d.id())));
        } else if patch.remove_cover {
            while !tag.pictures().is_empty() {
                tag.remove_picture(0);
            }
            report.written.push(Field::Cover.name());
        } else if let Some(cover) = &patch.cover {
            while !tag.pictures().is_empty() {
                tag.remove_picture(0);
            }
            use lofty::picture::{MimeType, Picture, PictureType};
            let mime = MimeType::from_str(cover.mime.as_str());
            tag.push_picture(
                Picture::unchecked(cover.data.clone())
                    .pic_type(PictureType::CoverFront)
                    .mime_type(mime)
                    .build(),
            );
            report.written.push(Field::Cover.name());
        }
    }

    tag.save_to_path(path, lofty::config::WriteOptions::default())
        .map_err(|e| format!("写入失败: {e}"))?;
    Ok(report)
}

/// 写入补丁：`None` = 不改，`Some("")` = 清空该字段。
#[derive(Default, Clone, Debug)]
pub struct MetaPatch {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub composer: Option<String>,
    pub genre: Option<String>,
    pub year: Option<u32>,
    pub track: Option<u32>,
    pub disc: Option<u32>,
    pub lyrics: Option<String>,
    pub comment: Option<String>,
    pub cover: Option<Cover>,
    /// 删除已有封面（`cover: None` 只表示「不改封面」）
    pub remove_cover: bool,
}

impl MetaPatch {
    pub fn is_empty(&self) -> bool {
        self.title.is_none()
            && self.artist.is_none()
            && self.album.is_none()
            && self.album_artist.is_none()
            && self.composer.is_none()
            && self.genre.is_none()
            && self.year.is_none()
            && self.track.is_none()
            && self.disc.is_none()
            && self.lyrics.is_none()
            && self.comment.is_none()
            && self.cover.is_none()
            && !self.remove_cover
    }

    /// 取某字段要写入的文本
    pub fn value(&self, f: Field) -> Option<String> {
        match f {
            Field::Title => self.title.clone(),
            Field::Artist => self.artist.clone(),
            Field::Album => self.album.clone(),
            Field::AlbumArtist => self.album_artist.clone(),
            Field::Composer => self.composer.clone(),
            Field::Genre => self.genre.clone(),
            Field::Year => self.year.map(|y| y.to_string()),
            Field::Track => self.track.map(|v| v.to_string()),
            Field::Disc => self.disc.map(|v| v.to_string()),
            Field::Lyrics => self.lyrics.clone(),
            Field::Comment => self.comment.clone(),
            Field::Cover => None,
        }
    }

    /// 哪些字段有改动（给 UI 提示用）
    #[allow(dead_code)]
    pub fn touched(&self) -> Vec<&'static str> {
        Field::TEXT_FIELDS
            .iter()
            .filter(|f| self.value(**f).is_some())
            .map(|f| f.name())
            .chain(self.cover.is_some().then_some(Field::Cover.name()))
            .collect()
    }
}

/// 从任意文本里抽出四位年份。
///
/// 能吃下 `1999`、`2003-07-31`、`2003/07/31`、`20030731`、`©2003`、
/// `1999.05`，甚至 `1/1/2003`（取第一个四位数字块）。
pub fn parse_year(text: &str) -> Option<u32> {
    let mut block = String::new();
    for ch in text.chars() {
        if ch.is_ascii_digit() {
            block.push(ch);
        } else {
            if let Some(y) = year_from_block(&block) {
                return Some(y);
            }
            block.clear();
        }
    }
    year_from_block(&block)
}

fn year_from_block(digits: &str) -> Option<u32> {
    if digits.len() < 4 {
        return None;
    }
    let head = &digits[..4];
    let year: u32 = head.parse().ok()?;
    if (1000..=2999).contains(&year) {
        Some(year)
    } else if digits.len() == 5 && digits.starts_with('0') {
        let y: u32 = digits[..4].parse().ok()?;
        (1000..=2999).contains(&y).then_some(y)
    } else {
        None
    }
}

/// `3/12` → (Some(3), Some(12))；`7` → (Some(7), None)
pub fn parse_num_pair(text: &str) -> (Option<u32>, Option<u32>) {
    let mut it = text.split('/');
    let first = it.next().and_then(|s| s.trim().parse::<u32>().ok());
    let second = it.next().and_then(|s| s.trim().parse::<u32>().ok());
    (first, second)
}

#[cfg(test)]
mod tests;
