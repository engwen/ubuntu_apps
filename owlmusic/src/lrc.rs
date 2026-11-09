//! LRC 歌词解析：支持多时间标签、offset、逐字标签、双行（原文/翻译）合并。

#[derive(Clone, Debug, Default)]
pub struct Line {
    pub time: u64,
    pub text: String,
    pub trans: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Lyrics {
    pub lines: Vec<Line>,
    /// 是否存在时间轴
    pub synced: bool,
    pub raw: String,
}

impl Lyrics {
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// 当前应高亮的行（返回下标）
    pub fn index_at(&self, ms: u64) -> Option<usize> {
        if self.lines.is_empty() {
            return None;
        }
        let idx = self.lines.partition_point(|l| l.time <= ms);
        if idx == 0 {
            Some(0)
        } else {
            Some(idx - 1)
        }
    }

    /// 序列化成 LRC 文本（用于写回标签/文件）
    pub fn to_lrc(&self) -> String {
        let mut s = String::new();
        for l in &self.lines {
            let m = l.time / 60_000;
            let sec = (l.time % 60_000) as f64 / 1000.0;
            s.push_str(&format!("[{:02}:{:05.2}]{}", m, sec, l.text));
            if let Some(t) = &l.trans {
                s.push_str(&format!(" / {}", t));
            }
            s.push('\n');
        }
        s
    }
}

fn parse_tag(tag: &str) -> Option<Result<u64, i64>> {
    let t = tag.trim();
    if let Some(rest) = t.strip_prefix("offset:") {
        return rest.trim().parse::<i64>().ok().map(Err);
    }
    let sep = t.find(':')?;
    let (m, rest) = t.split_at(sep);
    let m: u64 = m.trim().parse().ok()?;
    let rest = &rest[1..];
    let (s, frac) = match rest.find(['.', ':']) {
        Some(i) => (&rest[..i], &rest[i + 1..]),
        None => (rest, ""),
    };
    let s: u64 = s.trim().parse().ok()?;
    let frac: String = frac.chars().filter(|c| c.is_ascii_digit()).collect();
    let mut ms = 0u64;
    if !frac.is_empty() {
        let digits = frac.len().min(3);
        let v: u64 = frac[..digits].parse().ok()?;
        ms = match digits {
            1 => v * 100,
            2 => v * 10,
            _ => v,
        };
    }
    Some(Ok(m * 60_000 + s * 1000 + ms))
}

fn strip_word_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

pub fn parse(text: &str) -> Lyrics {
    let mut entries: Vec<Line> = Vec::new();
    let mut offset: i64 = 0;
    let mut plain: Vec<String> = Vec::new();
    let mut saw_time = false;

    for raw in text.lines() {
        let mut rest = raw.trim_start();
        let mut stamps: Vec<u64> = Vec::new();
        loop {
            if !rest.starts_with('[') {
                break;
            }
            let Some(end) = rest.find(']') else { break };
            let tag = &rest[1..end];
            match parse_tag(tag) {
                Some(Ok(ms)) => {
                    stamps.push(ms);
                    saw_time = true;
                    rest = &rest[end + 1..];
                }
                Some(Err(off)) => {
                    offset = off;
                    rest = &rest[end + 1..];
                }
                None => {
                    // [ti:xxx] 之类的元信息
                    rest = &rest[end + 1..];
                }
            }
        }
        let body = strip_word_tags(rest).trim().to_string();
        if stamps.is_empty() {
            if !body.is_empty() {
                plain.push(body);
            }
            continue;
        }
        for ms in stamps {
            entries.push(Line {
                time: ms,
                text: body.clone(),
                trans: None,
            });
        }
    }

    if !saw_time {
        // 纯文本歌词
        let lines = plain
            .into_iter()
            .map(|t| Line {
                time: 0,
                text: t,
                trans: None,
            })
            .collect();
        return Lyrics {
            lines,
            synced: false,
            raw: text.to_string(),
        };
    }

    entries.sort_by_key(|l| l.time);
    // 同一时间点的第二行视为翻译
    let mut merged: Vec<Line> = Vec::with_capacity(entries.len());
    for e in entries {
        if e.text.is_empty() {
            continue;
        }
        match merged.last_mut() {
            Some(last) if e.time.abs_diff(last.time) <= 40 => {
                if last.text != e.text {
                    match &mut last.trans {
                        Some(t) => {
                            if !t.contains(&e.text) {
                                t.push_str("  ");
                                t.push_str(&e.text);
                            }
                        }
                        None => last.trans = Some(e.text),
                    }
                }
            }
            _ => merged.push(e),
        }
    }

    if offset != 0 {
        for l in &mut merged {
            let t = l.time as i64 - offset;
            l.time = t.max(0) as u64;
        }
    }

    Lyrics {
        lines: merged,
        synced: true,
        raw: text.to_string(),
    }
}

/// 从纯文本猜是否有时间轴
pub fn looks_synced(text: &str) -> bool {
    text.lines().take(30).any(|l| {
        let t = l.trim_start();
        t.starts_with('[') && t.find(']').map(|e| t[1..e].contains(':')).unwrap_or(false)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_basic() {
        let l = parse("[00:12.34]hello\n[00:15.00]world\n");
        assert!(l.synced);
        assert_eq!(l.lines.len(), 2);
        assert_eq!(l.lines[0].time, 12_340);
        assert_eq!(l.lines[1].text, "world");
    }

    #[test]
    fn merges_translation_and_multi_stamp() {
        let l = parse("[00:10.00][00:20.00]repeat\n[00:30.00]line\n[00:30.00]译文\n");
        assert_eq!(l.lines.len(), 3);
        assert_eq!(l.lines[1].text, "repeat");
        assert_eq!(l.lines[2].trans.as_deref(), Some("译文"));
    }

    #[test]
    fn plain_text_is_unsynced() {
        let l = parse("just words\nno time\n");
        assert!(!l.synced);
        assert_eq!(l.lines.len(), 2);
    }

    #[test]
    fn offset_is_applied() {
        let l = parse("[offset:-500]\n[00:10.00]a\n");
        assert_eq!(l.lines[0].time, 10_500);
    }

    #[test]
    fn index_at_boundaries() {
        let l = parse("[00:01.00]a\n[00:02.00]b\n");
        assert_eq!(l.index_at(0), Some(0));
        assert_eq!(l.index_at(1_500), Some(0));
        assert_eq!(l.index_at(2_000), Some(1));
        assert_eq!(l.index_at(99_000), Some(1));
    }

    #[test]
    fn roundtrip_to_lrc() {
        let l = parse("[00:05.50]abc\n");
        let out = l.to_lrc();
        assert!(out.contains("[00:05.50]abc"));
        let again = parse(&out);
        assert_eq!(again.lines[0].time, 5_500);
    }

    #[test]
    fn detects_synced_text() {
        assert!(looks_synced("[ar:x]\n[00:01.00]hi"));
        assert!(!looks_synced("plain\nlyrics"));
    }
}
