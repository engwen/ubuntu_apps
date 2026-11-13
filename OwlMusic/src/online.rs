//! 在线元数据：iTunes Search（ACG / 日系覆盖好）、MusicBrainz 兜底、LRCLIB 歌词。

use serde_json::Value;
use std::time::Duration;

const UA: &str = "owlmusic/0.1 (https://github.com/owl/owlmusic)";

#[derive(Clone, Debug, Default)]
pub struct Candidate {
    pub source: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_artist: String,
    pub year: Option<u32>,
    pub track: Option<u32>,
    pub disc: Option<u32>,
    pub genre: Option<String>,
    pub cover_url: Option<String>,
    pub dur_ms: Option<u64>,
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(15))
        .user_agent(UA)
        .build()
}

pub fn fetch(url: &str) -> Result<Vec<u8>, String> {
    let resp = agent().get(url).call().map_err(|e| match e {
        ureq::Error::Status(c, _) => format!("HTTP {c}"),
        other => other.to_string(),
    })?;
    let mut buf: Vec<u8> = Vec::new();
    std::io::copy(&mut resp.into_reader(), &mut buf).map_err(|e| e.to_string())?;
    if buf.is_empty() {
        return Err("返回内容为空".into());
    }
    Ok(buf)
}

fn get_json(url: &str) -> Result<Value, String> {
    let resp = agent()
        .get(url)
        .set("Accept", "application/json")
        .call()
        .map_err(|e| match e {
            ureq::Error::Status(c, _) => format!("HTTP {c}"),
            other => other.to_string(),
        })?;
    resp.into_json::<Value>().map_err(|e| e.to_string())
}

pub fn pct(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn s(v: &Value, k: &str) -> String {
    v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string()
}

fn year_of(date: &str) -> Option<u32> {
    let y = date.get(0..4)?;
    y.parse().ok()
}

pub fn search_itunes(term: &str, country: &str, limit: u32) -> Result<Vec<Candidate>, String> {
    let url = format!(
        "https://itunes.apple.com/search?term={}&entity=song&limit={}&country={}",
        pct(term),
        limit,
        country
    );
    let v = get_json(&url)?;
    let mut out = Vec::new();
    if let Some(arr) = v.get("results").and_then(|r| r.as_array()) {
        for r in arr {
            let art = s(r, "artworkUrl100").replace("100x100bb", "600x600bb");
            let album = s(r, "collectionName");
            out.push(Candidate {
                source: "iTunes".into(),
                title: s(r, "trackName"),
                artist: s(r, "artistName"),
                album_artist: s(r, "artistName"),
                album,
                year: year_of(&s(r, "releaseDate")),
                track: r
                    .get("trackNumber")
                    .and_then(|x| x.as_u64())
                    .map(|x| x as u32),
                disc: r
                    .get("discNumber")
                    .and_then(|x| x.as_u64())
                    .map(|x| x as u32),
                genre: Some(s(r, "primaryGenreName")).filter(|g| !g.is_empty()),
                cover_url: if art.is_empty() { None } else { Some(art) },
                dur_ms: r.get("trackTimeMillis").and_then(|x| x.as_u64()),
            });
        }
    }
    Ok(out)
}

pub fn search_musicbrainz(term: &str, limit: u32) -> Result<Vec<Candidate>, String> {
    let url = format!(
        "https://musicbrainz.org/ws/2/recording?query={}&fmt=json&limit={}",
        pct(term),
        limit
    );
    let v = get_json(&url)?;
    let mut out = Vec::new();
    if let Some(arr) = v.get("recordings").and_then(|r| r.as_array()) {
        for r in arr {
            let artist = r
                .get("artist-credit")
                .and_then(|a| a.as_array())
                .map(|a| {
                    a.iter()
                        .map(|c| s(c, "name"))
                        .collect::<Vec<_>>()
                        .join(" & ")
                })
                .unwrap_or_default();
            let release = r
                .get("releases")
                .and_then(|x| x.as_array())
                .and_then(|a| a.first());
            let album = release.map(|x| s(x, "title")).unwrap_or_default();
            let year = release
                .and_then(|x| x.get("date"))
                .and_then(|d| d.as_str())
                .and_then(year_of);
            let track = release
                .and_then(|x| x.get("media"))
                .and_then(|m| m.as_array())
                .and_then(|a| a.first())
                .and_then(|m| m.get("track-number"))
                .and_then(|t| t.as_u64())
                .map(|t| t as u32);
            let release_id = release.and_then(|x| x.get("id")).and_then(|x| x.as_str());
            out.push(Candidate {
                source: "MusicBrainz".into(),
                title: s(r, "title"),
                artist: artist.clone(),
                album_artist: artist,
                album,
                year,
                track,
                disc: None,
                genre: None,
                cover_url: release_id
                    .map(|id| format!("https://coverartarchive.org/release/{id}/front-500")),
                dur_ms: r.get("length").and_then(|x| x.as_u64()),
            });
        }
    }
    Ok(out)
}

/// 综合搜索：iTunes 优先，结果太少时补 MusicBrainz
pub fn search_all(term: &str, country: &str) -> Result<Vec<Candidate>, String> {
    let mut out = search_itunes(term, country, 12).unwrap_or_default();
    if out.len() < 4 {
        if let Ok(mut mb) = search_musicbrainz(term, 8) {
            out.append(&mut mb);
        }
    }
    if out.is_empty() {
        return Err("没有找到匹配结果".into());
    }
    Ok(out)
}

#[derive(Clone, Debug, Default)]
pub struct LyricHit {
    pub lrc: String,
    pub synced: bool,
    pub source: String,
}

pub fn fetch_lyrics(
    artist: &str,
    title: &str,
    album: &str,
    dur_ms: u64,
) -> Result<LyricHit, String> {
    let dur = dur_ms / 1000;
    let get_url = format!(
        "https://lrclib.net/api/get?artist_name={}&track_name={}&album_name={}&duration={}",
        pct(artist),
        pct(title),
        pct(album),
        dur
    );
    if let Ok(v) = get_json(&get_url) {
        if let Some(hit) = hit_from_value(&v) {
            return Ok(hit);
        }
    }
    let search_url = format!(
        "https://lrclib.net/api/search?track_name={}&artist_name={}",
        pct(title),
        pct(artist)
    );
    if let Ok(v) = get_json(&search_url) {
        if let Some(arr) = v.as_array() {
            let mut best: Option<LyricHit> = None;
            for item in arr {
                if let Some(h) = hit_from_value(item) {
                    let better = match &best {
                        None => true,
                        Some(b) => !b.synced && h.synced,
                    };
                    if better {
                        best = Some(h);
                    }
                }
            }
            if let Some(b) = best {
                return Ok(b);
            }
        }
    }
    // 只给了曲名也再试一次
    let q_url = format!(
        "https://lrclib.net/api/search?q={}",
        pct(&format!("{artist} {title}"))
    );
    if let Ok(v) = get_json(&q_url) {
        if let Some(arr) = v.as_array() {
            for item in arr {
                if let Some(h) = hit_from_value(item) {
                    if h.synced {
                        return Ok(h);
                    }
                }
            }
        }
    }
    Err("LRCLIB 没有收录这首歌词".into())
}

fn hit_from_value(v: &Value) -> Option<LyricHit> {
    if v.get("instrumental")
        .and_then(|x| x.as_bool())
        .unwrap_or(false)
    {
        return None;
    }
    if let Some(synced) = v.get("syncedLyrics").and_then(|x| x.as_str()) {
        if !synced.trim().is_empty() {
            return Some(LyricHit {
                lrc: synced.replace("\r\n", "\n"),
                synced: true,
                source: "LRCLIB".into(),
            });
        }
    }
    if let Some(plain) = v.get("plainLyrics").and_then(|x| x.as_str()) {
        if !plain.trim().is_empty() {
            return Some(LyricHit {
                lrc: plain.replace("\r\n", "\n"),
                synced: false,
                source: "LRCLIB".into(),
            });
        }
    }
    None
}

// ================= 多条候选 =================
// 同一首歌在 LRCLIB 里往往有好几条记录（不同专辑、不同时长、有人传了逐行、
// 有人只传了纯文本）。以前只挑「第一条有时间轴的」，用户没得选；
// 这里把候选全部捞回来，交给界面列出来让用户自己挑。

#[derive(Clone, Debug, Default)]
pub struct LyricCandidate {
    /// LRCLIB 的记录 id，用来去重
    pub id: u64,
    pub title: String,
    pub artist: String,
    pub album: String,
    /// 这条歌词对应的音频时长（毫秒），用来判断跟本地文件是不是同一版
    pub dur_ms: Option<u64>,
    pub synced: bool,
    pub lrc: String,
    pub source: String,
}

impl LyricCandidate {
    /// 和本地曲目时长差多少毫秒；任一方没时长就给 None
    pub fn dur_gap(&self, target_ms: Option<u64>) -> Option<u64> {
        match (target_ms, self.dur_ms) {
            (Some(t), Some(d)) if t > 0 && d > 0 => Some(t.abs_diff(d)),
            _ => None,
        }
    }

    /// 预览用的前几行（去掉时间轴和空行）
    pub fn preview(&self, n: usize) -> String {
        let mut out: Vec<String> = Vec::new();
        for raw in self.lrc.lines() {
            let t = raw.trim();
            if t.is_empty() {
                continue;
            }
            // 逐行歌词里把 [mm:ss.xx] 前缀剥掉，预览只看文字
            let text = strip_stamp(t);
            if text.is_empty() {
                continue;
            }
            out.push(text);
            if out.len() >= n {
                break;
            }
        }
        out.join("\n")
    }

    pub fn line_count(&self) -> usize {
        self.lrc
            .lines()
            .filter(|l| !strip_stamp(l.trim()).is_empty())
            .count()
    }
}

fn strip_stamp(s: &str) -> String {
    let mut rest = s;
    while let Some(r) = rest.strip_prefix('[') {
        match r.find(']') {
            Some(i) => {
                if is_stamp(&r[..i]) {
                    rest = &r[i + 1..];
                } else {
                    break;
                }
            }
            None => break,
        }
    }
    rest.trim().to_string()
}

/// 方括号里是「时间轴」还是「元信息」？只有这两种才该剥掉，
/// 歌词正文里自带的 `[笑]`「[副歌]」不能动。
fn is_stamp(inner: &str) -> bool {
    if inner.is_empty() {
        return false;
    }
    // [00:12] [00:12.34] [01:02:03.45]
    let time_like = inner
        .chars()
        .next()
        .map(|c| c.is_ascii_digit())
        .unwrap_or(false)
        && inner
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, ':' | '.'));
    // [ti:标题] [ar:xxx] [by:xxx] [offset:+100] [length:03:45]
    let meta_like = match inner.find(':') {
        Some(i) if i > 0 && i <= 6 => {
            let key = &inner[..i];
            key.chars().all(|c| c.is_ascii_alphanumeric())
                && key.chars().any(|c| c.is_ascii_alphabetic())
        }
        _ => false,
    };
    time_like || meta_like
}

fn candidate_from_value(v: &Value) -> Option<LyricCandidate> {
    if v.get("instrumental")
        .and_then(|x| x.as_bool())
        .unwrap_or(false)
    {
        return None;
    }
    let gs = |k: &str| {
        v.get(k)
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .trim()
            .to_string()
    };
    let (lrc, synced) = match v.get("syncedLyrics").and_then(|x| x.as_str()) {
        Some(s) if !s.trim().is_empty() => (s.replace("\r\n", "\n"), true),
        _ => match v.get("plainLyrics").and_then(|x| x.as_str()) {
            Some(p) if !p.trim().is_empty() => (p.replace("\r\n", "\n"), false),
            _ => return None,
        },
    };
    Some(LyricCandidate {
        id: v.get("id").and_then(|x| x.as_u64()).unwrap_or(0),
        title: gs("trackName"),
        artist: gs("artistName"),
        album: gs("albumName"),
        dur_ms: v
            .get("duration")
            .and_then(|x| x.as_f64())
            .map(|s| (s * 1000.0).round() as u64)
            .filter(|d| *d > 0),
        synced,
        lrc,
        source: "LRCLIB".into(),
    })
}

/// 去重 + 排序：有时间轴的排前面，其次时长最接近本地文件的排前面。
/// 抽成纯函数，方便测。
fn tidy_candidates(list: &mut Vec<LyricCandidate>, target_ms: Option<u64>) {
    let mut seen_ids: Vec<u64> = Vec::new();
    let mut seen_texts: Vec<String> = Vec::new();
    list.retain(|c| {
        if c.id != 0 {
            if seen_ids.contains(&c.id) {
                return false;
            }
            seen_ids.push(c.id);
        }
        // 不同 id 但歌词正文一模一样（同一份歌词被重复投稿）也只留一条
        let key = format!(
            "{}\u{1}{}\u{1}{}",
            c.title.to_lowercase(),
            c.artist.to_lowercase(),
            c.lrc.trim()
        );
        if seen_texts.contains(&key) {
            return false;
        }
        seen_texts.push(key);
        true
    });
    list.sort_by(|a, b| {
        // 逐行优先：有时间轴才可能做卡拉OK
        b.synced.cmp(&a.synced).then_with(|| {
            let ga = a.dur_gap(target_ms).unwrap_or(u64::MAX / 2);
            let gb = b.dur_gap(target_ms).unwrap_or(u64::MAX / 2);
            ga.cmp(&gb)
                .then_with(|| b.line_count().cmp(&a.line_count()))
        })
    });
}

fn collect_into(url: &str, out: &mut Vec<LyricCandidate>, errs: &mut Vec<String>) {
    match get_json(url) {
        Ok(v) => {
            if let Some(arr) = v.as_array() {
                for item in arr {
                    if let Some(c) = candidate_from_value(item) {
                        out.push(c);
                    }
                }
            }
        }
        Err(e) => errs.push(e),
    }
}

fn finish(
    mut out: Vec<LyricCandidate>,
    target_ms: Option<u64>,
    errs: Vec<String>,
) -> Result<Vec<LyricCandidate>, String> {
    tidy_candidates(&mut out, target_ms);
    out.truncate(30);
    if out.is_empty() {
        return Err(errs
            .into_iter()
            .next()
            .unwrap_or_else(|| "LRCLIB 没有收录这首歌词".into()));
    }
    Ok(out)
}

/// 结构化检索：曲名 + 艺人 / 曲名 + 专辑，再补一次整串关键词。
pub fn search_lyrics(
    artist: &str,
    title: &str,
    album: &str,
    target_ms: Option<u64>,
) -> Result<Vec<LyricCandidate>, String> {
    let title = title.trim();
    let artist = artist.trim();
    let album = album.trim();
    if title.is_empty() && artist.is_empty() {
        return Err("曲名为空，没法搜歌词".into());
    }
    let mut out: Vec<LyricCandidate> = Vec::new();
    let mut errs: Vec<String> = Vec::new();
    if !title.is_empty() && !artist.is_empty() {
        collect_into(
            &format!(
                "https://lrclib.net/api/search?track_name={}&artist_name={}",
                pct(title),
                pct(artist)
            ),
            &mut out,
            &mut errs,
        );
    }
    if !title.is_empty() && !album.is_empty() {
        collect_into(
            &format!(
                "https://lrclib.net/api/search?track_name={}&album_name={}",
                pct(title),
                pct(album)
            ),
            &mut out,
            &mut errs,
        );
    }
    let q = format!("{artist} {title}").trim().to_string();
    if !q.is_empty() {
        collect_into(
            &format!("https://lrclib.net/api/search?q={}", pct(&q)),
            &mut out,
            &mut errs,
        );
    }
    finish(out, target_ms, errs)
}

/// 用户自己在搜索框里敲的关键词，直接整串丢给 LRCLIB。
pub fn search_lyrics_text(
    term: &str,
    target_ms: Option<u64>,
) -> Result<Vec<LyricCandidate>, String> {
    let term = term.trim();
    if term.is_empty() {
        return Err("请输入关键词".into());
    }
    let mut out: Vec<LyricCandidate> = Vec::new();
    let mut errs: Vec<String> = Vec::new();
    collect_into(
        &format!("https://lrclib.net/api/search?q={}", pct(term)),
        &mut out,
        &mut errs,
    );
    // 整串没结果时，按「艺人 曲名」再拆一次往往能命中
    if out.is_empty() {
        if let Some((a, t)) = term.split_once(" - ").or_else(|| term.split_once('—')) {
            collect_into(
                &format!(
                    "https://lrclib.net/api/search?track_name={}&artist_name={}",
                    pct(t.trim()),
                    pct(a.trim())
                ),
                &mut out,
                &mut errs,
            );
        }
    }
    finish(out, target_ms, errs)
}

#[cfg(test)]
mod candidate_tests {
    use super::*;

    fn c(
        id: u64,
        title: &str,
        artist: &str,
        dur: Option<u64>,
        synced: bool,
        lrc: &str,
    ) -> LyricCandidate {
        LyricCandidate {
            id,
            title: title.into(),
            artist: artist.into(),
            album: String::new(),
            dur_ms: dur,
            synced,
            lrc: lrc.into(),
            source: "LRCLIB".into(),
        }
    }

    #[test]
    fn stamps_are_stripped_for_preview() {
        assert_eq!(strip_stamp("[00:12.34]夜に駆ける"), "夜に駆ける");
        assert_eq!(strip_stamp("[00:12.34][00:40.10]副歌"), "副歌");
        assert_eq!(strip_stamp("[ti:标题]"), "");
        // 正文里本来就有方括号，不能当时间轴剥掉
        assert_eq!(strip_stamp("[笑] 才不是呢"), "[笑] 才不是呢");
    }

    #[test]
    fn preview_skips_empty_lines_and_counts() {
        let l = c(
            1,
            "t",
            "a",
            None,
            true,
            "[00:01.00]第一行\n\n[00:02.00]第二行\n[00:03.00]",
        );
        assert_eq!(l.preview(5), "第一行\n第二行");
        assert_eq!(l.line_count(), 2);
    }

    #[test]
    fn dedupe_and_order_put_synced_and_closest_first() {
        let mut v = vec![
            c(1, "歌", "唱", Some(200_000), false, "纯文本 A"),
            c(2, "歌", "唱", Some(301_000), true, "逐行 差得远"),
            c(3, "歌", "唱", Some(300_000), true, "逐行 正好"),
            // 同一条记录被两个查询都返回：只能留一份
            c(3, "歌", "唱", Some(300_000), true, "逐行 正好"),
            // 不同 id 但正文完全一样：也算重复
            c(9, "歌", "唱", Some(250_000), true, "逐行 正好"),
        ];
        tidy_candidates(&mut v, Some(300_000));
        assert_eq!(v.len(), 3);
        assert_eq!(v[0].lrc, "逐行 正好"); // 逐行 + 时长完全一致
        assert_eq!(v[1].lrc, "逐行 差得远");
        assert_eq!(v[2].lrc, "纯文本 A"); // 没有时间轴的排最后
    }

    #[test]
    fn dur_gap_handles_missing_values() {
        let a = c(1, "t", "a", Some(180_000), false, "x");
        assert_eq!(a.dur_gap(Some(182_000)), Some(2_000));
        let b = c(2, "t", "a", None, false, "y");
        assert_eq!(b.dur_gap(Some(182_000)), None);
        assert_eq!(b.dur_gap(None), None);
    }

    #[test]
    fn empty_term_is_rejected_before_any_request() {
        assert!(search_lyrics_text("   ", Some(1000)).is_err());
        assert!(search_lyrics("", "  ", "", None).is_err());
    }
}

#[cfg(test)]
mod probe_tests {
    use super::*;

    #[test]
    #[ignore]
    fn lyric_probe() {
        let url = format!(
            "https://lrclib.net/api/search?track_name={}&artist_name={}",
            pct("晴天"),
            pct("周杰伦")
        );
        match get_json(&url) {
            Ok(v) => println!("RAW ok, array len = {:?}", v.as_array().map(|a| a.len())),
            Err(e) => println!("RAW err = {e}"),
        }
        match fetch_lyrics("周杰伦", "晴天", "叶惠美", 269_000) {
            Ok(h) => println!(
                "HIT {} synced={} lines={}",
                h.source,
                h.synced,
                h.lrc.lines().count()
            ),
            Err(e) => println!("MISS {e}"),
        }
    }

    /// 真连一次网：确认多候选能捞到几条、排序对不对
    #[test]
    #[ignore]
    fn lyric_multi_probe() {
        match search_lyrics("周杰伦", "晴天", "叶惠美", Some(269_000)) {
            Ok(v) => {
                println!("候选 {} 条", v.len());
                for (i, c) in v.iter().take(6).enumerate() {
                    println!(
                        "  #{i} id={} {} / {} / {:?} synced={} {}行",
                        c.id,
                        c.title,
                        c.artist,
                        c.dur_ms,
                        c.synced,
                        c.line_count()
                    );
                }
            }
            Err(e) => println!("MISS {e}"),
        }
    }
}
