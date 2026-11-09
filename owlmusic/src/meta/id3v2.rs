//! ID3v2 方言（2.2 / 2.3 / 2.4）。
//!
//! 这是唯一需要自己实现写入器的格式。写入口只有 [`write_v24`] 一个：不管
//! 改的是曲名、年份、歌词还是整批在线匹配的字段，都把整张标签**重建为
//! ID3v2.4**，方便后续统一管理。具体做法是：
//!
//! 1. 解析出原标签的真实版本与全部帧；
//! 2. 把所有帧升级到 v2.4 的帧 ID 与帧头（v2.2 三字节 ID → 四字节，
//!    `PIC` → `APIC`，`TORY` → `TDOR`，`IPLS` → `TIPL`）；
//! 3. 各版本零散的年份写法（`TYE` / `TYER`+`TDAT`+`TIME` / `TDRC` /
//!    `TXXX:YEAR`）合并成一条 `TDRC`；
//! 4. 应用 patch，其余帧连帧体原样保留；
//! 5. 文件尾的 ID3v1 先并入（补空缺）再删除，只留一张 v2.4 标签。
//!
//! 帧 ID 对照：
//!
//! | 字段 | v2.2 | v2.3 | v2.4 |
//! |------|------|------|------|
//! | 曲名 | TT2 | TIT2 | TIT2 |
//! | 艺术家 | TP1 | TPE1 | TPE1 |
//! | 专辑 | TAL | TALB | TALB |
//! | 专辑艺术家 | TP2 | TPE2 | TPE2 |
//! | 作曲 | TCM | TCOM | TCOM |
//! | 流派 | TCO | TCON | TCON |
//! | 年份 | TYE | TYER | TDRC |
//! | 音轨 | TRK | TRCK | TRCK |
//! | 碟号 | TPA | TPOS | TPOS |
//! | 歌词 | ULT | USLT | USLT |
//! | 注释 | COM | COMM | COMM |
//! | 封面 | PIC | APIC | APIC |

use super::{
    parse_num_pair, parse_year, Cover, Field, MetaPatch, TagDialect, TrackMeta, WriteReport,
};
use lofty::tag::{ItemKey, TagType};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

pub static DIALECT: Id3v2 = Id3v2;

pub struct Id3v2;

impl TagDialect for Id3v2 {
    fn id(&self) -> &'static str {
        "ID3v2"
    }

    fn tag_type(&self) -> TagType {
        TagType::Id3v2
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
            ("TDRC", ItemKey::RecordingDate),
            ("TDOR", ItemKey::OriginalReleaseDate),
            ("TYER", ItemKey::Year),
        ]
    }

    fn markers(&self) -> &'static str {
        "TIT2/TPE1/TALB/TPE2/TCOM/TCON/TDRC(TYER/TYE)/TRCK/TPOS/USLT/COMM/APIC"
    }

    fn read_raw(&self, path: &Path) -> TrackMeta {
        read_all_raw(path)
    }

    fn write(&self, path: &Path, patch: &MetaPatch) -> Result<WriteReport, String> {
        write_v24(path, patch)
    }
}

/// 帧 ID ↔ 字段。`None` 表示该版本没有这个字段。
fn frame_id(major: u8, field: Field) -> Option<&'static str> {
    let v22 = major == 2;
    Some(match field {
        Field::Title => if v22 { "TT2" } else { "TIT2" },
        Field::Artist => if v22 { "TP1" } else { "TPE1" },
        Field::Album => if v22 { "TAL" } else { "TALB" },
        Field::AlbumArtist => if v22 { "TP2" } else { "TPE2" },
        Field::Composer => if v22 { "TCM" } else { "TCOM" },
        Field::Genre => if v22 { "TCO" } else { "TCON" },
        Field::Year => year_frame_id(major),
        Field::Track => if v22 { "TRK" } else { "TRCK" },
        Field::Disc => if v22 { "TPA" } else { "TPOS" },
        Field::Lyrics => if v22 { "ULT" } else { "USLT" },
        Field::Comment => if v22 { "COM" } else { "COMM" },
        Field::Cover => if v22 { "PIC" } else { "APIC" },
    })
}

fn year_frame_id(major: u8) -> &'static str {
    match major {
        2 => "TYE",
        3 => "TYER",
        _ => "TDRC",
    }
}

fn picture_frame_id(major: u8) -> &'static str {
    if major == 2 {
        "PIC"
    } else {
        "APIC"
    }
}

/// 是否是年份类 TXXX 描述（各家软件的自定义键名）
pub fn is_year_desc(desc: &str) -> bool {
    matches!(
        desc.trim().to_ascii_uppercase().as_str(),
        "YEAR"
            | "DATE"
            | "RELEASEDATE"
            | "RELEASE DATE"
            | "RELEASE YEAR"
            | "ORIGINALYEAR"
            | "ORIGINALDATE"
            | "发行年份"
            | "年份"
    )
}

// ================= 文件头解析 =================

/// 一个已解析的 ID3v2 标签。
#[allow(dead_code)]
pub struct TagInfo {
    pub major: u8,
    pub revision: u8,
    pub flags: u8,
    pub frames: Vec<RawFrame>,
    /// 标签体大小（不含 10 字节头、不含 footer）
    pub size: usize,
    /// 音频数据起点
    pub audio_start: usize,
}

/// 裸帧：帧 ID + 帧体（不含帧头）+ 原始字节（含帧头，改写时原样保留）
pub struct RawFrame {
    pub id: String,
    pub body: Vec<u8>,
}

impl TagInfo {
    /// 只读文件头与标签体，不碰音频，几十 MB 的文件也不会整个读进内存。
    pub fn read(path: &Path) -> Option<TagInfo> {
        let mut f = File::open(path).ok()?;
        let mut head = [0u8; 10];
        f.read_exact(&mut head).ok()?;
        if &head[..3] != b"ID3" {
            return None;
        }
        let major = head[3];
        if !(2..=4).contains(&major) {
            return None;
        }
        let revision = head[4];
        let flags = head[5];
        let size = synchsafe(&head[6..10])?;
        // 标签再大也不该超过 64MB；异常就当没有
        if size == 0 || size > 64 * 1024 * 1024 {
            return None;
        }
        let mut body = vec![0u8; size];
        f.read_exact(&mut body).ok()?;

        // 整个标签做了去同步（老软件常见）
        if flags & 0x80 != 0 {
            body = deunsync(&body);
        }
        let frames = parse_frames(&body, major, flags)?;
        let footer = if major == 4 && flags & 0x10 != 0 {
            10
        } else {
            0
        };
        Some(TagInfo {
            major,
            revision,
            flags,
            frames,
            size,
            audio_start: 10 + size + footer,
        })
    }
}

pub fn has_id3v2_header(path: &Path) -> bool {
    let mut head = [0u8; 3];
    File::open(path)
        .and_then(|mut f| f.read_exact(&mut head))
        .map(|_| &head == b"ID3")
        .unwrap_or(false)
}

// ================= 读取全部字段 =================

pub fn read_all_raw(path: &Path) -> TrackMeta {
    read_all_raw_opt(path).unwrap_or_default()
}

pub fn read_all_raw_opt(path: &Path) -> Option<TrackMeta> {
    let info = TagInfo::read(path)?;
    let major = info.major;
    let mut m = TrackMeta {
        dialect: Some("ID3v2"),
        version: Some(format!("ID3v2.{major}")),
        ..Default::default()
    };

    let text = |field: Field| -> Option<String> {
        let id = frame_id(major, field)?;
        info.frames
            .iter()
            .find(|f| f.id == id)
            .map(|f| decode_text(&f.body))
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    };

    m.title = text(Field::Title);
    m.artist = text(Field::Artist);
    m.album = text(Field::Album);
    m.album_artist = text(Field::AlbumArtist);
    m.composer = text(Field::Composer);
    m.genre = text(Field::Genre);
    if let Some(v) = text(Field::Track) {
        let (n, total) = parse_num_pair(&v);
        m.track = n;
        m.track_total = total;
    }
    if let Some(v) = text(Field::Disc) {
        let (n, total) = parse_num_pair(&v);
        m.disc = n;
        m.disc_total = total;
    }

    // 年份：先正统帧，再 TXXX 私货
    for id in year_frame_ids(major) {
        if let Some(f) = info.frames.iter().find(|f| f.id == *id) {
            if let Some(y) = parse_year(&decode_text(&f.body)) {
                m.year = Some(y);
                m.year_marker = Some((*id).to_string());
                break;
            }
        }
    }
    if m.year.is_none() {
        let txxx = if major == 2 { "TXX" } else { "TXXX" };
        for f in info.frames.iter().filter(|f| f.id == txxx) {
            let Some((desc, value)) = split_kv(&f.body) else {
                continue;
            };
            if is_year_desc(&desc) {
                if let Some(y) = parse_year(&value) {
                    m.year = Some(y);
                    m.year_marker = Some(format!("TXXX:{}", desc.trim()));
                    break;
                }
            }
        }
    }

    // 歌词 / 注释
    let lyr_id = frame_id(major, Field::Lyrics).unwrap();
    if let Some(f) = info.frames.iter().find(|f| f.id == lyr_id) {
        m.lyrics = decode_lang_frame(&f.body);
    }
    let cmt_id = frame_id(major, Field::Comment).unwrap();
    if let Some(f) = info.frames.iter().find(|f| f.id == cmt_id) {
        m.comment = decode_lang_frame(&f.body);
    }

    // 封面
    let pic_id = picture_frame_id(major);
    if let Some(f) = info.frames.iter().find(|f| f.id == pic_id) {
        m.cover = decode_apic(&f.body, major);
    }

    Some(m)
}

fn year_frame_ids(major: u8) -> &'static [&'static str] {
    match major {
        2 => &["TYE", "TOR", "TRD"],
        3 => &["TYER", "TYDR", "TDOR", "TORY"],
        _ => &["TDRC", "TDOR", "TORY", "TYER"],
    }
}

// ================= 写入：统一重建为 ID3v2.4 =================

/// 一个待写出的 v2.4 帧。
struct OutFrame {
    id: String,
    body: Vec<u8>,
}

/// 写入的**唯一**入口：把整张标签重建为 ID3v2.4。
///
/// 任何字段的修改（单改曲名、单改年份、在线匹配整批写入、只写歌词……）都走
/// 这一条路，不存在「只改年份才保真」的特殊分支：
///
/// 1. 所有帧升级到 v2.4 的帧 ID 与帧头（synchsafe 长度）：v2.2 的 `PIC`
///    转成 `APIC`，v2.4 已废弃的 `TSIZ`/`RVAD`/`EQUA` 丢弃并报告；
/// 2. 各版本的年份写法（`TYE` / `TYER`+`TDAT`+`TIME` / `TDRC` /
///    `TXXX:YEAR`）合并成一条 `TDRC`，原 `TDRC` 带完整日期时保留日期；
/// 3. 应用 patch 里的字段，其余帧原样保留（只换帧头，不动内容）；
/// 4. ID3v1 独有的字段先并入 v2.4，然后删除 ID3v1，文件里只留一张标签；
/// 5. 音频数据原样拷贝，不留 padding。
pub fn write_v24(path: &Path, patch: &MetaPatch) -> Result<WriteReport, String> {
    let info = TagInfo::read(path);
    // 有 ID3 头但解析不出来（奇怪的去同步/扩展头/长度不符）时，
    // 宁可拒绝也不要把它当成音频塞到新标签后面，那会把文件写坏。
    if info.is_none() && has_id3v2_header(path) {
        return Err(
            "这个文件的 ID3v2 标签结构无法解析（可能带未知的扩展头或去同步），\
             为避免写坏文件已放弃写入"
                .into(),
        );
    }

    let audio_start = info.as_ref().map(|i| i.audio_start).unwrap_or(0);
    let len = std::fs::metadata(path)
        .map_err(|e| format!("读取文件信息失败: {e}"))?
        .len() as usize;
    let v1 = read_v1_tail(path);
    let audio_end = if v1.is_some() {
        len.saturating_sub(128)
    } else {
        len
    };
    if audio_start > audio_end {
        return Err("标签长度超出文件大小，文件可能已损坏".into());
    }

    let mut report = WriteReport::new("ID3v2.4");
    match &info {
        Some(i) if i.major != 4 => report
            .notes
            .push(format!("已从 ID3v2.{} 升级为 ID3v2.4", i.major)),
        None if v1.is_some() => report.notes.push("已从 ID3v1 升级为 ID3v2.4".into()),
        _ => {}
    }

    // ---- 1. 升级已有帧，顺手把年份的多种写法合并 ----
    let mut frames: Vec<OutFrame> = Vec::new();
    let mut dropped: Vec<String> = Vec::new();
    let mut year = YearParts::default();
    let mut txxx_year: Option<String> = None;

    if let Some(info) = &info {
        for f in &info.frames {
            if is_year_frame_id(&f.id) {
                year.take(&f.id, &decode_text(&f.body));
                continue;
            }
            if f.id == "TXXX" || f.id == "TXX" {
                if let Some((desc, val)) = split_kv(&f.body) {
                    if is_year_desc(&desc) {
                        if txxx_year.is_none() {
                            txxx_year = Some(val);
                        }
                        continue; // 私货合并进 TDRC，不再单独保留
                    }
                }
            }
            match upgrade_frame(info.major, f) {
                Some((id, body)) => frames.push(OutFrame { id, body }),
                None => dropped.push(f.id.clone()),
            }
        }
    }

    let mut year_text = year.compose().or(txxx_year);
    if let Some(want) = patch.year {
        // 原 TDRC 已经是这一年（可能还带月日）就别把它削成四位数字
        let keep = year_text.as_deref().and_then(parse_year) == Some(want);
        if !keep {
            year_text = Some(want.to_string());
        }
    }

    // ---- 2. 应用 patch：所有字段都走同一段循环 ----
    for field in Field::TEXT_FIELDS {
        let Some(v) = patch.value(field) else { continue };
        if field == Field::Year {
            report.written.push(field.name());
            continue;
        }
        let id = frame_id(4, field).unwrap_or_default();
        // 只改音轨号时别把 "3/10" 里的总数弄丢
        let old_total = frames
            .iter()
            .find(|f| f.id == id)
            .and_then(|f| parse_num_pair(&decode_text(&f.body)).1);
        frames.retain(|f| f.id != id);
        report.written.push(field.name());
        if v.trim().is_empty() {
            continue; // 清空字段 = 删帧（上面已经删了）
        }
        let mut text = v;
        if (field == Field::Track || field == Field::Disc) && !text.contains('/') {
            if let Some(total) = old_total {
                text = format!("{text}/{total}");
            }
        }
        let body = match field {
            Field::Lyrics | Field::Comment => lang_body("chi", &text),
            _ => text_body(&text),
        };
        frames.push(OutFrame {
            id: id.to_string(),
            body,
        });
    }

    if let Some(text) = year_text.filter(|s| !s.trim().is_empty()) {
        frames.retain(|f| f.id != "TDRC");
        frames.push(OutFrame {
            id: "TDRC".into(),
            body: text_body(&text),
        });
    }

    // ---- 3. 封面 ----
    if patch.remove_cover {
        frames.retain(|f| f.id != "APIC");
        report.written.push(Field::Cover.name());
    } else if let Some(c) = &patch.cover {
        frames.retain(|f| f.id != "APIC");
        frames.push(OutFrame {
            id: "APIC".into(),
            body: apic_body(c),
        });
        report.written.push(Field::Cover.name());
    }

    // ---- 4. ID3v1 独有的信息并入后，删掉 ID3v1 ----
    if let Some(v1) = &v1 {
        merge_v1(&mut frames, v1, patch);
        report
            .notes
            .push("已移除文件尾的 ID3v1，只保留 ID3v2.4".into());
    }

    if !dropped.is_empty() {
        dropped.sort();
        dropped.dedup();
        report
            .notes
            .push(format!("已丢弃 ID3v2.4 不再支持的帧：{}", dropped.join("、")));
    }

    // ---- 5. 写出：v2.4 头 + 全部帧 + 原样音频 ----
    let mut body = Vec::new();
    for f in &frames {
        body.extend_from_slice(&frame24(&f.id, &f.body));
    }

    let mut header = Vec::with_capacity(10);
    header.extend_from_slice(b"ID3");
    header.push(4); // 版本：永远是 v2.4
    header.push(0); // 修订号
    header.push(0); // 无去同步、无扩展头、无 footer
    header.extend_from_slice(&enc_synchsafe(body.len()));

    let tmp = tmp_sibling(path);
    {
        let mut src = File::open(path).map_err(|e| format!("打开失败: {e}"))?;
        let mut dst = File::create(&tmp).map_err(|e| format!("创建临时文件失败: {e}"))?;
        dst.write_all(&header).map_err(|e| format!("写入失败: {e}"))?;
        dst.write_all(&body).map_err(|e| format!("写入失败: {e}"))?;

        src.seek(SeekFrom::Start(audio_start as u64))
            .map_err(|e| format!("定位失败: {e}"))?;
        let mut audio = src.take((audio_end - audio_start) as u64);
        std::io::copy(&mut audio, &mut dst).map_err(|e| format!("拷贝音频失败: {e}"))?;
    }

    // 保留原文件权限
    if let Ok(meta) = std::fs::metadata(path) {
        let _ = std::fs::set_permissions(&tmp, meta.permissions());
    }
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("替换原文件失败: {e}")
    })?;

    Ok(report)
}

fn tmp_sibling(path: &Path) -> std::path::PathBuf {
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "audio".into());
    path.with_file_name(format!(".{name}.owlmusic.tmp"))
}

// ---------- 年份合并 ----------

/// 各版本里描述「录制时间」的帧。合并成一条 v2.4 的 `TDRC`。
fn is_year_frame_id(id: &str) -> bool {
    matches!(
        id,
        // v2.4 / v2.3 / v2.2 的年份与零散日期时间
        "TDRC" | "TYER" | "TYE" | "TDAT" | "TDA" | "TIME" | "TIM" | "TRDA" | "TRD"
    )
}

#[derive(Default)]
struct YearParts {
    /// v2.4 的完整写法（可能带月日时分）
    full: Option<String>,
    /// 只有年份
    year: Option<String>,
    /// `TDAT`：DDMM
    date: Option<String>,
    /// `TIME`：HHMM
    time: Option<String>,
}

impl YearParts {
    fn take(&mut self, id: &str, text: &str) {
        let t = text.trim();
        if t.is_empty() {
            return;
        }
        match id {
            // TDAT / TIME 只存 DDMM / HHMM，本身不含年份，不能拿 parse_year 过滤
            "TDAT" | "TDA" => {
                if self.date.is_none() {
                    self.date = Some(t.to_string());
                }
            }
            "TIME" | "TIM" => {
                if self.time.is_none() {
                    self.time = Some(t.to_string());
                }
            }
            "TDRC" => {
                if self.full.is_none() && parse_year(t).is_some() {
                    self.full = Some(t.to_string());
                }
            }
            _ => {
                if self.year.is_none() && parse_year(t).is_some() {
                    self.year = Some(t.to_string());
                }
            }
        }
    }

    fn compose(&self) -> Option<String> {
        if let Some(full) = &self.full {
            return Some(full.clone());
        }
        let year = self.year.as_deref().and_then(parse_year)?;
        let mut s = year.to_string();
        if let Some(d) = self.date.as_deref().map(digits_of) {
            if d.len() >= 4 {
                let (day, month) = (&d[0..2], &d[2..4]);
                if month != "00" {
                    s.push_str(&format!("-{month}-{day}"));
                }
            }
        }
        if let Some(t) = self.time.as_deref().map(digits_of) {
            if t.len() >= 4 {
                s.push_str(&format!("T{}:{}", &t[0..2], &t[2..4]));
            }
        }
        Some(s)
    }
}

fn digits_of(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_digit()).collect()
}

// ---------- 帧升级 ----------

/// 把一帧升级成 v2.4：换帧 ID，必要时换帧体结构。
/// 返回 `None` 表示 v2.4 里没有等价写法，只能丢弃。
fn upgrade_frame(major: u8, f: &RawFrame) -> Option<(String, Vec<u8>)> {
    if major == 2 {
        let id = v22_to_v24(&f.id)?;
        let body = if f.id == "PIC" {
            pic_to_apic(&f.body)?
        } else {
            f.body.clone()
        };
        Some((id.to_string(), body))
    } else {
        match f.id.as_str() {
            // v2.4 已废弃，或帧体结构变了（RVA2/EQU2 与 RVAD/EQUA 不兼容）
            "TSIZ" | "RVAD" | "EQUA" => None,
            // 原始发行日期：v2.3 的 TORY 在 v2.4 里叫 TDOR，语义保留
            "TORY" | "TDOR" => Some(("TDOR".into(), f.body.clone())),
            // 参与人员：v2.3 的 IPLS 在 v2.4 里并入 TIPL
            "IPLS" => Some(("TIPL".into(), f.body.clone())),
            other => Some((other.to_string(), f.body.clone())),
        }
    }
}

/// ID3v2.2 的三字节帧 ID → v2.4 的四字节帧 ID。
fn v22_to_v24(id: &str) -> Option<&'static str> {
    Some(match id {
        "TT1" => "TIT1",
        "TT2" => "TIT2",
        "TT3" => "TIT3",
        "TP1" => "TPE1",
        "TP2" => "TPE2",
        "TP3" => "TPE3",
        "TP4" => "TPE4",
        "TCM" => "TCOM",
        "TXT" => "TEXT",
        "TLA" => "TLAN",
        "TCO" => "TCON",
        "TAL" => "TALB",
        "TRK" => "TRCK",
        "TPA" => "TPOS",
        "TRC" => "TSRC",
        "TBP" => "TBPM",
        "TKE" => "TKEY",
        "TOT" => "TOAL",
        "TOA" => "TOPE",
        "TOF" => "TOFN",
        "TOL" => "TOLY",
        "TOR" => "TDOR",
        "TCR" => "TCOP",
        "TPB" => "TPUB",
        "TEN" => "TENC",
        "TSS" => "TSSE",
        "TXX" => "TXXX",
        "WXX" => "WXXX",
        "WAF" => "WOAF",
        "WAR" => "WOAR",
        "WAS" => "WOAS",
        "WCM" => "WCOM",
        "WCP" => "WCOP",
        "WPB" => "WPUB",
        "IPL" => "TIPL",
        "MCI" => "MCDI",
        "ETC" => "ETCO",
        "MLL" => "MLLT",
        "STC" => "SYTC",
        "ULT" => "USLT",
        "SLT" => "SYLT",
        "COM" => "COMM",
        "REV" => "RVRB",
        "PIC" => "APIC",
        "GEO" => "GEOB",
        "CNT" => "PCNT",
        "POP" => "POPM",
        "BUF" => "RBUF",
        "CRA" => "AENC",
        "LNK" => "LINK",
        _ => return None, // TSI / RVA / EQU 等：结构变了或已废弃
    })
}

/// v2.2 的 `PIC` → v2.4 的 `APIC`：3 字节图片格式换成 MIME 字符串。
fn pic_to_apic(body: &[u8]) -> Option<Vec<u8>> {
    if body.len() < 5 {
        return None;
    }
    let enc = body[0];
    let mime = match &body[1..4] {
        b"PNG" => "image/png",
        b"GIF" => "image/gif",
        b"BMP" => "image/bmp",
        _ => "image/jpeg",
    };
    let mut out = Vec::with_capacity(body.len() + mime.len());
    out.push(enc);
    out.extend_from_slice(mime.as_bytes());
    out.push(0);
    out.extend_from_slice(&body[4..]); // 图片类型 + 描述 + 数据
    Some(out)
}

// ---------- 帧构造（全部按 v2.4 规范） ----------

/// 文本帧体：v2.4 支持 UTF-8，中文比 UTF-16 更省空间。
fn text_body(text: &str) -> Vec<u8> {
    let mut v = vec![3u8];
    v.extend_from_slice(text.as_bytes());
    v
}

/// USLT / COMM：编码 + 3 字节语言 + 描述 + 终止符 + 正文
fn lang_body(lang: &str, text: &str) -> Vec<u8> {
    let mut body = vec![3u8];
    let lang = lang.as_bytes();
    body.extend_from_slice(&lang[..3.min(lang.len())]);
    body.push(0); // 空描述
    body.extend_from_slice(text.as_bytes());
    body
}

/// APIC：编码 + MIME + 0 + 图片类型 + 描述 + 数据
fn apic_body(cover: &Cover) -> Vec<u8> {
    let mime = Cover::mime_guess(&cover.data);
    let mut body = vec![0u8]; // Latin-1，MIME 与空描述都是 ASCII
    body.extend_from_slice(mime.as_bytes());
    body.push(0);
    body.push(3); // Cover (front)
    body.push(0); // 空描述
    body.extend_from_slice(&cover.data);
    body
}

fn frame24(id: &str, body: &[u8]) -> Vec<u8> {
    let mut v = id.as_bytes().to_vec();
    v.extend_from_slice(&enc_synchsafe(body.len()));
    v.extend_from_slice(&[0, 0]);
    v.extend_from_slice(body);
    v
}

// ---------- ID3v1：并入后删除 ----------

/// 文件尾 128 字节的 ID3v1 / v1.1。
struct V1Tag {
    title: String,
    artist: String,
    album: String,
    year: Option<u32>,
    comment: String,
    track: Option<u32>,
}

pub fn has_id3v1_tail(path: &Path) -> bool {
    read_v1_tail(path).is_some()
}

fn read_v1_tail(path: &Path) -> Option<V1Tag> {
    let len = std::fs::metadata(path).ok()?.len() as usize;
    if len < 128 {
        return None;
    }
    let mut f = File::open(path).ok()?;
    f.seek(SeekFrom::Start((len - 128) as u64)).ok()?;
    let mut b = [0u8; 128];
    f.read_exact(&mut b).ok()?;
    if &b[..3] != b"TAG" {
        return None;
    }
    let decode = |s: &[u8]| -> String {
        let end = s.iter().position(|&c| c == 0).unwrap_or(s.len());
        s[..end].iter().map(|&c| c as char).collect()
    };
    let year = decode(&b[93..97])
        .trim()
        .parse::<u32>()
        .ok()
        .filter(|y| (1000..=2999).contains(y));
    // ID3v1.1：注释第 28 字节为 0、第 29 字节是音轨号
    let (comment, track) = if b[125] == 0 && b[126] != 0 {
        (decode(&b[97..125]), Some(b[126] as u32))
    } else {
        (decode(&b[97..127]), None)
    };
    Some(V1Tag {
        title: decode(&b[3..33]),
        artist: decode(&b[33..63]),
        album: decode(&b[63..93]),
        year,
        comment,
        track,
    })
}

/// 把 ID3v1 独有的内容补进 v2.4，避免删掉 v1 时丢信息。
/// patch 里明确要改/清空的字段不补。
fn merge_v1(frames: &mut Vec<OutFrame>, v1: &V1Tag, patch: &MetaPatch) {
    fn add(frames: &mut Vec<OutFrame>, id: &str, text: &str) {
        if text.trim().is_empty() || frames.iter().any(|f| f.id == id) {
            return;
        }
        frames.push(OutFrame {
            id: id.to_string(),
            body: text_body(text),
        });
    }

    if patch.title.is_none() {
        add(frames, "TIT2", &v1.title);
    }
    if patch.artist.is_none() {
        add(frames, "TPE1", &v1.artist);
    }
    if patch.album.is_none() {
        add(frames, "TALB", &v1.album);
    }
    if patch.year.is_none() {
        if let Some(y) = v1.year {
            add(frames, "TDRC", &y.to_string());
        }
    }
    if patch.track.is_none() {
        if let Some(t) = v1.track {
            add(frames, "TRCK", &t.to_string());
        }
    }
    if patch.comment.is_none()
        && !v1.comment.trim().is_empty()
        && !frames.iter().any(|f| f.id == "COMM")
    {
        frames.push(OutFrame {
            id: "COMM".into(),
            body: lang_body("chi", &v1.comment),
        });
    }
}

// ================= 原始帧解析 =================

fn parse_frames(body: &[u8], major: u8, flags: u8) -> Option<Vec<RawFrame>> {
    let mut pos = 0usize;
    // 扩展头
    if flags & 0x40 != 0 {
        match major {
            3 => {
                // v2.3：4 字节大小（不含自身）
                let ext = be32(body.get(0..4)?)? as usize;
                pos = 4 + ext;
            }
            4 => {
                // v2.4：synchsafe 大小（含自身）
                pos = synchsafe(body.get(0..4)?)?;
            }
            _ => return None,
        }
    }

    let mut frames = Vec::new();
    while pos < body.len() {
        if body[pos] == 0 {
            break; // 进入 padding
        }
        if major == 2 {
            if pos + 6 > body.len() {
                break;
            }
            let id_bytes = &body[pos..pos + 3];
            if !is_frame_id(id_bytes) {
                break;
            }
            let id = String::from_utf8_lossy(id_bytes).to_string();
            let len = ((body[pos + 3] as usize) << 16)
                | ((body[pos + 4] as usize) << 8)
                | body[pos + 5] as usize;
            let start = pos + 6;
            let end = start.checked_add(len)?;
            if end > body.len() {
                break;
            }
            frames.push(RawFrame {
                id,
                body: body[start..end].to_vec(),
            });
            pos = end;
        } else {
            if pos + 10 > body.len() {
                break;
            }
            let id_bytes = &body[pos..pos + 4];
            if !is_frame_id(id_bytes) {
                break;
            }
            let id = String::from_utf8_lossy(id_bytes).to_string();
            let len = if major == 4 {
                synchsafe(&body[pos + 4..pos + 8])?
            } else {
                be32(&body[pos + 4..pos + 8])? as usize
            };
            let fflags = u16::from_be_bytes([body[pos + 8], body[pos + 9]]);
            let start = pos + 10;
            let end = start.checked_add(len)?;
            if end > body.len() {
                break;
            }
            // 压缩/加密/分组的帧直接跳过（文本帧几乎不会这样）
            let skip = if major == 4 {
                fflags & 0x000C != 0
            } else {
                fflags & 0x00C0 != 0
            };
            if !skip {
                frames.push(RawFrame {
                    id,
                    body: body[start..end].to_vec(),
                });
            }
            pos = end;
        }
    }
    Some(frames)
}

/// 帧 ID 只能是 A-Z 和 0-9（`TIT2`、`TDRC` 都带数字）
fn is_frame_id(b: &[u8]) -> bool {
    b.iter()
        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
}

/// 编码：把长度写成 4 字节 synchsafe
fn enc_synchsafe(n: usize) -> [u8; 4] {
    [
        ((n >> 21) & 0x7F) as u8,
        ((n >> 14) & 0x7F) as u8,
        ((n >> 7) & 0x7F) as u8,
        (n & 0x7F) as u8,
    ]
}

fn synchsafe(b: &[u8]) -> Option<usize> {
    if b.len() < 4 {
        return None;
    }
    if b[..4].iter().any(|x| x & 0x80 != 0) {
        return None;
    }
    Some(((b[0] as usize) << 21) | ((b[1] as usize) << 14) | ((b[2] as usize) << 7) | b[3] as usize)
}

fn be32(b: &[u8]) -> Option<u32> {
    Some(u32::from_be_bytes([
        *b.first()?,
        *b.get(1)?,
        *b.get(2)?,
        *b.get(3)?,
    ]))
}

/// 去同步：`0xFF 0x00` → `0xFF`
fn deunsync(b: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        out.push(b[i]);
        if b[i] == 0xFF && i + 1 < b.len() && b[i + 1] == 0x00 {
            i += 2;
        } else {
            i += 1;
        }
    }
    out
}

// ================= 文本解码 =================

/// 按编码字节解出文本帧内容
fn decode_text(b: &[u8]) -> String {
    let Some((&enc, rest)) = b.split_first() else {
        return String::new();
    };
    decode_with_encoding(enc, rest)
}

fn decode_with_encoding(enc: u8, bytes: &[u8]) -> String {
    match enc {
        1 => decode_utf16(bytes, true),
        2 => decode_utf16(bytes, false),
        3 => String::from_utf8_lossy(bytes)
            .trim_end_matches('\0')
            .to_string(),
        // 0 = ISO-8859-1。老中文文件实际常塞 GBK，这里按 Latin-1 逐字节映射；
        // 对纯数字年份完全够用，中文会显示成扩展拉丁字符（原样保留，不丢字节）
        _ => bytes
            .iter()
            .take_while(|&&c| c != 0)
            .map(|&c| c as char)
            .collect(),
    }
}

fn decode_utf16(bytes: &[u8], detect_bom: bool) -> String {
    let mut be = false;
    let mut body = bytes;
    if detect_bom {
        if bytes.starts_with(&[0xFF, 0xFE]) {
            body = &bytes[2..];
        } else if bytes.starts_with(&[0xFE, 0xFF]) {
            body = &bytes[2..];
            be = true;
        }
    }
    let units: Vec<u16> = body
        .chunks_exact(2)
        .map(|c| {
            if be {
                u16::from_be_bytes([c[0], c[1]])
            } else {
                u16::from_le_bytes([c[0], c[1]])
            }
        })
        .take_while(|&u| u != 0)
        .collect();
    String::from_utf16_lossy(&units)
}

/// 描述 + 正文（`TXXX`、`USLT` 去掉语言后的部分）
fn split_at_term(enc: u8, bytes: &[u8]) -> Option<(String, String)> {
    if enc == 1 || enc == 2 {
        let mut i = 0;
        while i + 1 < bytes.len() {
            if bytes[i] == 0 && bytes[i + 1] == 0 {
                return Some((
                    decode_with_encoding(enc, &bytes[..i]),
                    decode_with_encoding(enc, &bytes[i + 2..]),
                ));
            }
            i += 2;
        }
        None
    } else {
        let cut = bytes.iter().position(|&c| c == 0)?;
        Some((
            decode_with_encoding(enc, &bytes[..cut]),
            decode_with_encoding(enc, &bytes[cut + 1..]),
        ))
    }
}

/// 拆 `TXXX`（描述 + 内容）
fn split_kv(body: &[u8]) -> Option<(String, String)> {
    let (&enc, rest) = body.split_first()?;
    split_at_term(enc, rest)
}

/// USLT / COMM：编码 + 语言 + 描述 + 正文
fn decode_lang_frame(body: &[u8]) -> Option<String> {
    if body.len() < 4 {
        return None;
    }
    let enc = body[0];
    let rest = &body[4..];
    let (_, text) = split_at_term(enc, rest)?;
    let text = text.trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// APIC / PIC
fn decode_apic(body: &[u8], major: u8) -> Option<Cover> {
    let enc = *body.first()?;
    let (mime, mut pos) = if major == 2 {
        let fmt = body.get(1..4)?;
        let mime = match fmt {
            b"PNG" => "image/png",
            b"GIF" => "image/gif",
            _ => "image/jpeg",
        };
        (mime.to_string(), 4)
    } else {
        let end = body[1..].iter().position(|&c| c == 0)? + 1;
        let mime = String::from_utf8_lossy(&body[1..end]).to_string();
        (mime, end + 1)
    };
    // 图片类型
    let _pic_type = *body.get(pos)?;
    pos += 1;
    // 描述
    let desc_len = if enc == 1 || enc == 2 {
        let mut i = pos;
        let mut found = None;
        while i + 1 < body.len() {
            if body[i] == 0 && body[i + 1] == 0 {
                found = Some(i + 2);
                break;
            }
            i += 2;
        }
        found?
    } else {
        body[pos..]
            .iter()
            .position(|&c| c == 0)
            .map(|i| pos + i + 1)?
    };
    let data = body.get(desc_len..)?.to_vec();
    if data.is_empty() {
        return None;
    }
    Some(Cover { data, mime })
}
