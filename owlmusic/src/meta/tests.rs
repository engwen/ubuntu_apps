//! 标签方言层的回归测试。
//!
//! 样本全部**现场合成**（ID3v2.2/2.3/2.4 + 最小 MPEG 帧 + ID3v1），不依赖
//! 本地音乐库，换台机器也能跑。重点验证两件事：
//!
//! 1. 所有字段（曲名/艺术家/专辑/作曲/年份/音轨/碟号/歌词/封面）都走同一套
//!    「先判方言、再按记号读写」的路径；
//! 2. 写入保持原标签版本与无关帧，不因为改一个字段就把整张标签翻新。

use super::*;
use crate::tags::MetaPatch;
use std::path::PathBuf;

// ---------- 造样本 ----------

fn synchsafe(n: usize) -> [u8; 4] {
    [
        ((n >> 21) & 0x7F) as u8,
        ((n >> 14) & 0x7F) as u8,
        ((n >> 7) & 0x7F) as u8,
        (n & 0x7F) as u8,
    ]
}

/// 帧体：编码字节 + 文本（UTF-8）
fn text_body(s: &str) -> Vec<u8> {
    let mut v = vec![3u8];
    v.extend_from_slice(s.as_bytes());
    v
}

fn utf16_body(s: &str) -> Vec<u8> {
    let mut v = vec![1u8, 0xFF, 0xFE];
    for u in s.encode_utf16() {
        v.extend_from_slice(&u.to_le_bytes());
    }
    v
}

fn frame23(id: &str, body: &[u8]) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(id.as_bytes());
    v.extend_from_slice(&(body.len() as u32).to_be_bytes());
    v.extend_from_slice(&[0, 0]);
    v.extend_from_slice(body);
    v
}

fn frame24(id: &str, body: &[u8]) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(id.as_bytes());
    v.extend_from_slice(&synchsafe(body.len()));
    v.extend_from_slice(&[0, 0]);
    v.extend_from_slice(body);
    v
}

fn frame22(id: &str, body: &[u8]) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(id.as_bytes());
    let n = body.len();
    v.extend_from_slice(&[(n >> 16) as u8, (n >> 8) as u8, n as u8]);
    v.extend_from_slice(body);
    v
}

fn txxx24(desc: &str, val: &str, utf16: bool) -> Vec<u8> {
    let mut body = Vec::new();
    if utf16 {
        body.push(1);
        body.extend_from_slice(&[0xFF, 0xFE]);
        for u in desc.encode_utf16() {
            body.extend_from_slice(&u.to_le_bytes());
        }
        body.extend_from_slice(&[0, 0]);
        body.extend_from_slice(&[0xFF, 0xFE]);
        for u in val.encode_utf16() {
            body.extend_from_slice(&u.to_le_bytes());
        }
    } else {
        body.push(3);
        body.extend_from_slice(desc.as_bytes());
        body.push(0);
        body.extend_from_slice(val.as_bytes());
    }
    frame24("TXXX", &body)
}

/// USLT / COMM 的帧体：编码 + 语言 + 描述 + 正文（不含帧头）
fn lang_body(lang: &str, desc: &str, text: &str) -> Vec<u8> {
    let mut body = vec![3u8];
    body.extend_from_slice(lang.as_bytes());
    body.extend_from_slice(desc.as_bytes());
    body.push(0);
    body.extend_from_slice(text.as_bytes());
    body
}

fn fake_jpeg() -> Vec<u8> {
    let mut v = vec![0xFF, 0xD8, 0xFF, 0xE0];
    v.extend_from_slice(&[0x11u8; 2048]);
    v.extend_from_slice(&[0xFF, 0xD9]);
    v
}

/// APIC 的帧体（不含帧头）
fn apic_body(data: &[u8]) -> Vec<u8> {
    let mut body = vec![0u8];
    body.extend_from_slice(b"image/jpeg\0");
    body.push(3);
    body.push(0);
    body.extend_from_slice(data);
    body
}

/// PIC（v2.2）的帧体
fn pic_body(data: &[u8]) -> Vec<u8> {
    let mut body = vec![0u8];
    body.extend_from_slice(b"JPG");
    body.push(3);
    body.push(0);
    body.extend_from_slice(data);
    body
}

fn tag(version: u8, frames: &[Vec<u8>]) -> Vec<u8> {
    let body: Vec<u8> = frames.concat();
    let mut v = Vec::new();
    v.extend_from_slice(b"ID3");
    v.push(version);
    v.push(0);
    v.push(0);
    v.extend_from_slice(&synchsafe(body.len()));
    v.extend_from_slice(&body);
    v
}

/// 最小合法 MPEG1 Layer3 帧：128kbps / 44.1kHz，每帧 417 字节
fn mpeg_frames(n: usize) -> Vec<u8> {
    let mut out = Vec::new();
    for _ in 0..n {
        let mut f = vec![0u8; 417];
        f[..4].copy_from_slice(&[0xFF, 0xFB, 0x90, 0x00]);
        out.extend_from_slice(&f);
    }
    out
}

fn id3v1(title: &str, artist: &str, album: &str, year: &str) -> Vec<u8> {
    fn pad(s: &str, n: usize) -> Vec<u8> {
        let mut b = s.as_bytes().to_vec();
        b.truncate(n);
        b.resize(n, 0);
        b
    }
    let mut v = b"TAG".to_vec();
    v.extend(pad(title, 30));
    v.extend(pad(artist, 30));
    v.extend(pad(album, 30));
    v.extend(pad(year, 4));
    v.extend(pad("", 30));
    v.push(0xFF);
    v
}

fn tmp_path(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("owlmusic-meta-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

fn write_sample(name: &str, bytes: &[u8]) -> PathBuf {
    let p = tmp_path(name);
    std::fs::write(&p, bytes).unwrap();
    p
}

fn mp3(tag_bytes: &[u8], v1: Option<Vec<u8>>) -> Vec<u8> {
    let mut v = tag_bytes.to_vec();
    v.extend(mpeg_frames(12));
    if let Some(t) = v1 {
        v.extend(t);
    }
    v
}

fn meta_of(p: &std::path::Path) -> TrackMeta {
    read_meta_from_path(p)
}

fn apply(p: &std::path::Path, patch: MetaPatch) -> WriteReport {
    write_meta(p, &patch).unwrap_or_else(|e| panic!("写入失败: {e}"))
}

// ---------- 文本解析 ----------

#[test]
fn parse_year_formats() {
    assert_eq!(parse_year("1999"), Some(1999));
    assert_eq!(parse_year("2003-07-31"), Some(2003));
    assert_eq!(parse_year("2003/07/31"), Some(2003));
    assert_eq!(parse_year("20030731"), Some(2003));
    assert_eq!(parse_year("©2003"), Some(2003));
    assert_eq!(parse_year("1/1/2003"), Some(2003));
    assert_eq!(parse_year("  2010  "), Some(2010));
    assert_eq!(parse_year("0000"), None);
    assert_eq!(parse_year("99"), None);
    assert_eq!(parse_year(""), None);
    assert_eq!(parse_year("未知"), None);
}

#[test]
fn parse_track_pair() {
    assert_eq!(parse_num_pair("3/12"), (Some(3), Some(12)));
    assert_eq!(parse_num_pair("7"), (Some(7), None));
    assert_eq!(parse_num_pair(""), (None, None));
}

// ---------- 读取：一种方言，全部字段 ----------

const FAKE_JPG: &[u8] = &[0xFF, 0xD8, 0xFF, 0xE0];

#[test]
fn reads_id3v23_all_fields() {
    let jpg = fake_jpeg();
    let t = tag(
        3,
        &[
            frame23("TIT2", &text_body("晴天")),
            frame23("TPE1", &text_body("周杰伦")),
            frame23("TALB", &text_body("叶惠美")),
            frame23("TPE2", &text_body("周杰伦")),
            frame23("TCOM", &text_body("周杰伦")),
            frame23("TCON", &text_body("Pop")),
            frame23("TYER", &text_body("2003")),
            frame23("TRCK", &text_body("3/10")),
            frame23("TPOS", &text_body("1/1")),
            frame23("USLT", &lang_body("chi", "", "[00:01.00]故事的小黄花")),
            frame23("APIC", &apic_body(&jpg)),
        ],
    );
    let p = write_sample("v23all.mp3", &mp3(&t, None));
    let m = meta_of(&p);

    assert_eq!(m.title.as_deref(), Some("晴天"));
    assert_eq!(m.artist.as_deref(), Some("周杰伦"));
    assert_eq!(m.album.as_deref(), Some("叶惠美"));
    assert_eq!(m.album_artist.as_deref(), Some("周杰伦"));
    assert_eq!(m.composer.as_deref(), Some("周杰伦"), "作曲必须走 TCOM");
    assert_eq!(m.genre.as_deref(), Some("Pop"));
    assert_eq!(m.year, Some(2003));
    assert_eq!(m.track, Some(3));
    assert_eq!(m.track_total, Some(10));
    assert_eq!(m.disc, Some(1));
    assert!(m.lyrics.as_deref().unwrap_or("").contains("小黄花"));
    assert_eq!(m.cover.as_ref().map(|c| c.data.len()), Some(jpg.len()));
    assert_eq!(m.version.as_deref(), Some("ID3v2.3"));
}

#[test]
fn reads_id3v24_tdrc() {
    let t = tag(
        4,
        &[
            frame24("TIT2", &text_body("标题")),
            frame24("TDRC", &text_body("2003-07-31")),
            frame24("TCOM", &text_body("作曲人")),
        ],
    );
    let p = write_sample("v24read.mp3", &mp3(&t, None));
    let m = meta_of(&p);
    assert_eq!(m.year, Some(2003));
    assert_eq!(m.year_marker.as_deref(), Some("TDRC"));
    assert_eq!(m.composer.as_deref(), Some("作曲人"));
}

#[test]
fn reads_id3v24_txxx_year() {
    let t = tag(
        4,
        &[
            frame24("TIT2", &text_body("标题")),
            txxx24("YEAR", "2001", false),
        ],
    );
    let p = write_sample("readtxxx.mp3", &mp3(&t, None));
    let m = meta_of(&p);
    assert_eq!(m.year, Some(2001));
    assert_eq!(m.year_marker.as_deref(), Some("TXXX:YEAR"));
}

#[test]
fn reads_txxx_year_utf16() {
    let t = tag(
        4,
        &[frame24("TIT2", &utf16_body("标题")), txxx24("年份", "2007", true)],
    );
    let p = write_sample("readtxxx16.mp3", &mp3(&t, None));
    assert_eq!(meta_of(&p).year, Some(2007));
}

#[test]
fn reads_id3v22_all_fields() {
    let jpg = fake_jpeg();
    let t = tag(
        2,
        &[
            frame22("TT2", &text_body("v22 标题")),
            frame22("TP1", &text_body("v22 艺人")),
            frame22("TAL", &text_body("v22 专辑")),
            frame22("TP2", &text_body("v22 专辑艺人")),
            frame22("TCM", &text_body("v22 作曲")),
            frame22("TCO", &text_body("Rock")),
            frame22("TYE", &text_body("1987")),
            frame22("TRK", &text_body("2/8")),
            frame22("ULT", &lang_body("eng", "", "v22 lyrics")),
            frame22("PIC", &pic_body(&jpg)),
        ],
    );
    let p = write_sample("v22all.mp3", &mp3(&t, None));
    let m = meta_of(&p);
    assert_eq!(m.title.as_deref(), Some("v22 标题"));
    assert_eq!(m.artist.as_deref(), Some("v22 艺人"));
    assert_eq!(m.album.as_deref(), Some("v22 专辑"));
    assert_eq!(m.composer.as_deref(), Some("v22 作曲"));
    assert_eq!(m.year, Some(1987));
    assert_eq!(m.track, Some(2));
    assert!(m.lyrics.as_deref().unwrap_or("").contains("v22 lyrics"));
    assert!(m.cover.is_some());
}

#[test]
fn reads_id3v1_only() {
    let p = write_sample(
        "readv1.mp3",
        &mp3(b"", Some(id3v1("v1 Title", "v1 Artist", "v1 Album", "1995"))),
    );
    let m = meta_of(&p);
    assert_eq!(m.title.as_deref(), Some("v1 Title"));
    assert_eq!(m.year, Some(1995));
    assert_eq!(m.version.as_deref(), Some("ID3v1"));
}

/// v2 里缺的字段要能往 ID3v1 兜底——老文件最常见的情况。
#[test]
fn falls_back_to_id3v1_per_field() {
    let t = tag(3, &[frame23("TIT2", &text_body("v2 标题"))]);
    let p = write_sample(
        "fallback.mp3",
        &mp3(&t, Some(id3v1("v1 Title", "v1 Artist", "v1 Album", "1990"))),
    );
    let m = meta_of(&p);
    // v2 有标题，就用 v2 的
    assert_eq!(m.title.as_deref(), Some("v2 标题"));
    // v2 没有艺术家/专辑/年份，落到 v1
    assert_eq!(m.artist.as_deref(), Some("v1 Artist"));
    assert_eq!(m.album.as_deref(), Some("v1 Album"));
    assert_eq!(m.year, Some(1990));
}

// ---------- 写入：一律升级为 ID3v2.4 ----------

#[test]
fn writes_v23_upgrades_to_v24() {
    let t = tag(
        3,
        &[
            frame23("TIT2", &text_body("旧标题")),
            frame23("TYER", &text_body("1998")),
        ],
    );
    let p = write_sample("write23.mp3", &mp3(&t, None));

    let r = apply(
        &p,
        MetaPatch {
            title: Some("新标题".into()),
            year: Some(2011),
            ..Default::default()
        },
    );
    assert!(r.written.contains(&"曲名"));
    assert!(r.written.contains(&"年份"));
    assert!(
        r.notes.iter().any(|n| n.contains("ID3v2.3")),
        "报告应说明升级：{}",
        r.summary()
    );

    let m = meta_of(&p);
    assert_eq!(m.title.as_deref(), Some("新标题"));
    assert_eq!(m.year, Some(2011));
    assert_eq!(m.version.as_deref(), Some("ID3v2.4"));

    let bytes = std::fs::read(&p).unwrap();
    assert_eq!(bytes[3], 4, "写入后必须升级到 v2.4");
    assert!(
        bytes.windows(4).any(|w| w == b"TDRC"),
        "v2.4 里年份应当写 TDRC"
    );
    assert!(
        !bytes.windows(4).any(|w| w == b"TYER"),
        "旧版本帧 TYER 不应残留"
    );
}

#[test]
fn writes_id3v24_tdrc() {
    let t = tag(4, &[frame24("TIT2", &text_body("标题"))]);
    let p = write_sample("write24.mp3", &mp3(&t, None));
    apply(
        &p,
        MetaPatch {
            year: Some(2019),
            ..Default::default()
        },
    );
    let bytes = std::fs::read(&p).unwrap();
    assert_eq!(bytes[3], 4);
    assert!(bytes.windows(4).any(|w| w == b"TDRC"));
    assert_eq!(meta_of(&p).year, Some(2019));
}

#[test]
fn writes_composer_upgrades_both() {
    for (ver, name) in [(3u8, "comp23.mp3"), (4, "comp24.mp3")] {
        let t = if ver == 3 {
            tag(3, &[frame23("TIT2", &text_body("标题"))])
        } else {
            tag(4, &[frame24("TIT2", &text_body("标题"))])
        };
        let p = write_sample(name, &mp3(&t, None));
        apply(
            &p,
            MetaPatch {
                composer: Some("林迈可".into()),
                ..Default::default()
            },
        );
        let m = meta_of(&p);
        assert_eq!(m.composer.as_deref(), Some("林迈可"));
        let b = std::fs::read(&p).unwrap();
        assert_eq!(b[3], 4, "v{ver} 写入后都应当是 v2.4");
        assert!(b.windows(4).any(|w| w == b"TCOM"));
    }
}

#[test]
fn writes_lyrics_into_uslt() {
    let t = tag(3, &[frame23("TIT2", &text_body("标题"))]);
    let p = write_sample("lyr23.mp3", &mp3(&t, None));
    apply(
        &p,
        MetaPatch {
            lyrics: Some("[00:01.00]第一行\n[00:05.00]第二行".into()),
            ..Default::default()
        },
    );
    let m = meta_of(&p);
    assert!(m.lyrics.unwrap().contains("第二行"));
    let b = std::fs::read(&p).unwrap();
    assert!(b.windows(4).any(|w| w == b"USLT"));
    assert_eq!(b[3], 4);
}

/// 改一个字段不能弄丢别的帧（封面这种大帧最容易被写坏）。
#[test]
fn writes_year_keeps_cover_and_title() {
    let jpg = fake_jpeg();
    let t = tag(
        3,
        &[
            frame23("TIT2", &text_body("别被写坏")),
            frame23("APIC", &apic_body(&jpg)),
            frame23("TYER", &text_body("1998")),
        ],
    );
    let p = write_sample("keepframes.mp3", &mp3(&t, None));

    apply(
        &p,
        MetaPatch {
            year: Some(2013),
            ..Default::default()
        },
    );

    let m = meta_of(&p);
    assert_eq!(m.year, Some(2013));
    assert_eq!(m.title.as_deref(), Some("别被写坏"));
    let cover = m.cover.expect("封面帧不能丢");
    assert_eq!(cover.data, jpg);
}

#[test]
fn replaces_and_removes_cover() {
    let old = vec![0xFF, 0xD8, 0xFF, 0xD9];
    let t = tag(
        3,
        &[
            frame23("TIT2", &text_body("封面测试")),
            frame23("APIC", &apic_body(&old)),
        ],
    );
    let p = write_sample("cover.mp3", &mp3(&t, None));

    let new = fake_jpeg();
    apply(
        &p,
        MetaPatch {
            cover: Some(Cover {
                data: new.clone(),
                mime: "image/jpeg".into(),
            }),
            ..Default::default()
        },
    );
    assert_eq!(meta_of(&p).cover.unwrap().data, new);

    apply(
        &p,
        MetaPatch {
            remove_cover: true,
            ..Default::default()
        },
    );
    assert!(meta_of(&p).cover.is_none(), "删除后不应再有封面");
    assert_eq!(meta_of(&p).title.as_deref(), Some("封面测试"));
}

/// 只有 ID3v1 的文件：升级成 v2.4，v1 独有的字段先并进来，再把 v1 删掉。
#[test]
fn upgrades_id3v1_only_file() {
    let p = write_sample(
        "writev1.mp3",
        &mp3(b"", Some(id3v1("v1 Title", "v1 Artist", "v1 Album", "1995"))),
    );

    let r = apply(
        &p,
        MetaPatch {
            title: Some("New Title".into()),
            year: Some(2004),
            ..Default::default()
        },
    );
    assert!(r.notes.iter().any(|n| n.contains("ID3v1")), "{}", r.summary());

    let m = meta_of(&p);
    assert_eq!(m.title.as_deref(), Some("New Title"));
    assert_eq!(m.artist.as_deref(), Some("v1 Artist"), "v1 独有的字段要并进来");
    assert_eq!(m.album.as_deref(), Some("v1 Album"));
    assert_eq!(m.year, Some(2004));
    assert_eq!(m.version.as_deref(), Some("ID3v2.4"));

    let bytes = std::fs::read(&p).unwrap();
    assert_eq!(bytes[3], 4);
    assert_ne!(
        &bytes[bytes.len() - 128..bytes.len() - 125],
        b"TAG",
        "ID3v1 应当被移除"
    );

    // 中文现在写得进 v2.4（以前 ID3v1 会拒绝）
    let r2 = apply(
        &p,
        MetaPatch {
            artist: Some("中文歌手".into()),
            ..Default::default()
        },
    );
    assert!(r2.written.contains(&"艺术家"));
    assert_eq!(meta_of(&p).artist.as_deref(), Some("中文歌手"));
}

/// 同时有 v2 与 v1 时：v1 唯一的信息并入 v2.4，然后删掉 v1，只留一张标签。
#[test]
fn removes_id3v1_after_merging() {
    let t = tag(3, &[frame23("TIT2", &text_body("Old"))]);
    let p = write_sample(
        "syncv1.mp3",
        &mp3(&t, Some(id3v1("Old", "Artist", "Album", "1990"))),
    );
    apply(
        &p,
        MetaPatch {
            title: Some("Fresh".into()),
            ..Default::default()
        },
    );

    let m = meta_of(&p);
    assert_eq!(m.title.as_deref(), Some("Fresh"));
    assert_eq!(m.artist.as_deref(), Some("Artist"), "v1 的艺术家不能丢");
    assert_eq!(m.album.as_deref(), Some("Album"));
    assert_eq!(m.year, Some(1990));

    let bytes = std::fs::read(&p).unwrap();
    assert_eq!(bytes[3], 4);
    assert_ne!(
        &bytes[bytes.len() - 128..bytes.len() - 125],
        b"TAG",
        "统一管理后不该再留 ID3v1"
    );
}

#[test]
fn rejects_absurd_year() {
    let t = tag(4, &[frame24("TIT2", &text_body("标题"))]);
    let p = write_sample("bad.mp3", &mp3(&t, None));
    assert!(write_meta(
        &p,
        &MetaPatch {
            year: Some(12),
            ..Default::default()
        }
    )
    .is_err());
}

/// 清空字段：空字符串表示删掉该帧，而不是写一个空串。
#[test]
fn clears_field() {
    let t = tag(
        3,
        &[
            frame23("TIT2", &text_body("标题")),
            frame23("TCOM", &text_body("作曲")),
        ],
    );
    let p = write_sample("clear.mp3", &mp3(&t, None));
    apply(
        &p,
        MetaPatch {
            composer: Some(String::new()),
            ..Default::default()
        },
    );
    assert!(meta_of(&p).composer.is_none());
    assert_eq!(meta_of(&p).title.as_deref(), Some("标题"));
}

// ---------- FLAC（有样本才跑） ----------

#[test]
fn flac_all_fields_roundtrip() {
    let raw = std::path::Path::new("target/testmusic/周杰伦 - 晴天.flac");
    if !raw.exists() {
        return;
    }
    let dst = tmp_path("roundtrip.flac");
    std::fs::copy(raw, &dst).unwrap();

    let r = apply(
        &dst,
        MetaPatch {
            title: Some("晴天".into()),
            artist: Some("周杰伦".into()),
            album: Some("叶惠美".into()),
            composer: Some("周杰伦".into()),
            year: Some(2003),
            genre: Some("Pop".into()),
            track: Some(3),
            lyrics: Some("[00:01.00]测试歌词".into()),
            ..Default::default()
        },
    );
    assert!(!r.written.is_empty());

    let m = meta_of(&dst);
    assert_eq!(m.title.as_deref(), Some("晴天"));
    assert_eq!(m.composer.as_deref(), Some("周杰伦"));
    assert_eq!(m.year, Some(2003));
    assert_eq!(m.track, Some(3));
    assert!(m.lyrics.unwrap().contains("测试歌词"));
    assert_eq!(m.version.as_deref(), Some("VorbisComment"));
}

/// 一次写入所有字段（在线匹配就是这个场景）：所有字段都必须落到 v2.4 的
/// 对应帧里，且原 v2.3 标签整体升级（不留 TYER 这类旧帧）。
#[test]
fn writes_all_fields_at_once_v23() {
    let t = tag(3, &[frame23("TIT2", &text_body("旧标题"))]);
    let p = write_sample("all23.mp3", &mp3(&t, None));
    let jpg = fake_jpeg();

    let r = apply(
        &p,
        MetaPatch {
            title: Some("新标题".into()),
            artist: Some("新艺人".into()),
            album: Some("新专辑".into()),
            album_artist: Some("新专辑艺人".into()),
            composer: Some("新作曲".into()),
            genre: Some("Rock".into()),
            year: Some(2020),
            track: Some(7),
            disc: Some(2),
            lyrics: Some("[00:03.00]新歌词".into()),
            comment: Some("备注".into()),
            cover: Some(Cover {
                data: jpg.clone(),
                mime: "image/jpeg".into(),
            }),
            ..Default::default()
        },
    );
    assert_eq!(r.written.len(), 12, "报告：{}", r.summary());

    let m = meta_of(&p);
    assert_eq!(m.title.as_deref(), Some("新标题"));
    assert_eq!(m.artist.as_deref(), Some("新艺人"));
    assert_eq!(m.album.as_deref(), Some("新专辑"));
    assert_eq!(m.album_artist.as_deref(), Some("新专辑艺人"));
    assert_eq!(m.composer.as_deref(), Some("新作曲"));
    assert_eq!(m.genre.as_deref(), Some("Rock"));
    assert_eq!(m.year, Some(2020));
    assert_eq!(m.track, Some(7));
    assert_eq!(m.disc, Some(2));
    assert!(m.lyrics.as_deref().unwrap_or("").contains("新歌词"));
    assert_eq!(m.cover.as_ref().map(|c| c.data.as_slice()), Some(jpg.as_slice()));

    let bytes = std::fs::read(&p).unwrap();
    assert_eq!(bytes[3], 4, "整批写入同样升级为 v2.4");
    for id in [
        b"TIT2", b"TPE1", b"TALB", b"TPE2", b"TCOM", b"TCON", b"TDRC", b"TRCK", b"TPOS", b"USLT",
        b"COMM", b"APIC",
    ] {
        assert!(
            bytes.windows(4).any(|w| w == id),
            "缺少帧 {}",
            String::from_utf8_lossy(id)
        );
    }
    assert!(
        !bytes.windows(4).any(|w| w == b"TYER"),
        "v2.3 的 TYER 应当已经升级成 TDRC"
    );
}

/// v2.2 → v2.4：三字节帧 ID 与 PIC 都要转成 v2.4 的写法。
#[test]
fn upgrades_id3v22_frames() {
    let jpg = fake_jpeg();
    let t = tag(
        2,
        &[
            frame22("TT2", &text_body("v22 标题")),
            frame22("TP1", &text_body("v22 艺人")),
            frame22("TCO", &text_body("Rock")),
            frame22("TYE", &text_body("1987")),
            frame22("ULT", &lang_body("eng", "", "[00:01.00]v22 lyrics")),
            frame22("PIC", &pic_body(&jpg)),
        ],
    );
    let p = write_sample("up22.mp3", &mp3(&t, None));

    let r = apply(
        &p,
        MetaPatch {
            album: Some("v22 专辑".into()),
            ..Default::default()
        },
    );
    assert!(r.notes.iter().any(|n| n.contains("ID3v2.2")), "{}", r.summary());

    let m = meta_of(&p);
    assert_eq!(m.title.as_deref(), Some("v22 标题"));
    assert_eq!(m.artist.as_deref(), Some("v22 艺人"));
    assert_eq!(m.album.as_deref(), Some("v22 专辑"));
    assert_eq!(m.genre.as_deref(), Some("Rock"));
    assert_eq!(m.year, Some(1987));
    assert!(m.lyrics.as_deref().unwrap_or("").contains("v22 lyrics"));
    assert_eq!(m.cover.as_ref().map(|c| c.data.as_slice()), Some(jpg.as_slice()));
    assert_eq!(m.version.as_deref(), Some("ID3v2.4"));

    let bytes = std::fs::read(&p).unwrap();
    assert_eq!(bytes[3], 4);
    assert!(bytes.windows(4).any(|w| w == b"APIC"), "PIC 要转成 APIC");
    assert!(bytes.windows(4).any(|w| w == b"TDRC"));
}

/// v2.3 的 TYER + TDAT + TIME 要合并成一条完整的 TDRC。
#[test]
fn merges_v23_date_frames_into_tdrc() {
    let t = tag(
        3,
        &[
            frame23("TIT2", &text_body("标题")),
            frame23("TYER", &text_body("2003")),
            frame23("TDAT", &text_body("3107")), // DDMM
            frame23("TIME", &text_body("1230")), // HHMM
        ],
    );
    let p = write_sample("date23.mp3", &mp3(&t, None));
    apply(
        &p,
        MetaPatch {
            title: Some("新标题".into()),
            ..Default::default()
        },
    );

    let m = meta_of(&p);
    assert_eq!(m.year, Some(2003));
    let bytes = std::fs::read(&p).unwrap();
    assert!(
        bytes.windows(4).any(|w| w == b"TDRC"),
        "年份要写成 TDRC"
    );
    // 合并出来的完整时间戳
    assert!(
        bytes
            .windows("2003-07-31T12:30".len())
            .any(|w| w == b"2003-07-31T12:30"),
        "应保留月日与时间"
    );
}

/// 原 TDRC 带完整日期时，只改别的字段不能把日期削成四位年份。
#[test]
fn keeps_full_tdrc_when_year_untouched() {
    let t = tag(
        4,
        &[
            frame24("TIT2", &text_body("标题")),
            frame24("TDRC", &text_body("2003-07-31")),
        ],
    );
    let p = write_sample("keepdate.mp3", &mp3(&t, None));
    apply(
        &p,
        MetaPatch {
            title: Some("新标题".into()),
            ..Default::default()
        },
    );
    let bytes = std::fs::read(&p).unwrap();
    assert!(bytes
        .windows("2003-07-31".len())
        .any(|w| w == b"2003-07-31"));
}

/// 改音轨号时保留原有的总数（"3/10" 不该变成 "7"）。
#[test]
fn keeps_track_total() {
    let t = tag(
        3,
        &[
            frame23("TIT2", &text_body("标题")),
            frame23("TRCK", &text_body("3/10")),
        ],
    );
    let p = write_sample("tracktotal.mp3", &mp3(&t, None));
    apply(
        &p,
        MetaPatch {
            track: Some(7),
            ..Default::default()
        },
    );
    assert_eq!(meta_of(&p).track, Some(7));
    assert_eq!(meta_of(&p).track_total, Some(10));
}

/// 真实 MP3（有样本才跑）：只改一个字段，整张标签也要升级成 v2.4，
/// 原有字段一个都不能丢。
#[test]
fn real_mp3_upgrades_to_v24() {
    let raw = std::path::Path::new("target/testmusic/某人 - MP3 测试.mp3");
    if !raw.exists() {
        return;
    }
    let dst = tmp_path("real.mp3");
    std::fs::copy(raw, &dst).unwrap();

    let before_meta = meta_of(&dst);
    let before = std::fs::read(&dst).unwrap();

    let r = apply(
        &dst,
        MetaPatch {
            composer: Some("测试作曲".into()),
            ..Default::default()
        },
    );

    let after = std::fs::read(&dst).unwrap();
    println!(
        "before v{} {} 字节 / after v{} {} 字节 —— {}",
        before[3],
        before.len(),
        after[3],
        after.len(),
        r.summary()
    );

    assert_eq!(after[3], 4, "真实 MP3 也要升级到 v2.4");
    let m = meta_of(&dst);
    assert_eq!(m.composer.as_deref(), Some("测试作曲"));
    assert_eq!(m.title, before_meta.title, "原有标题不能丢");
    assert_eq!(m.artist, before_meta.artist);
    assert_eq!(m.album, before_meta.album);
    assert_eq!(m.version.as_deref(), Some("ID3v2.4"));
}
