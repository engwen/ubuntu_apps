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
                track: r.get("trackNumber").and_then(|x| x.as_u64()).map(|x| x as u32),
                disc: r.get("discNumber").and_then(|x| x.as_u64()).map(|x| x as u32),
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
                        Some(b) => (!b.synced && h.synced),
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
    let q_url = format!("https://lrclib.net/api/search?q={}", pct(&format!("{artist} {title}")));
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
    if v.get("instrumental").and_then(|x| x.as_bool()).unwrap_or(false) {
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
            Ok(h) => println!("HIT {} synced={} lines={}", h.source, h.synced, h.lrc.lines().count()),
            Err(e) => println!("MISS {e}"),
        }
    }
}
