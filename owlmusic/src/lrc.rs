//! LRC 歌词解析：支持多时间标签、offset、逐字标签、双行（原文/翻译）合并。

#[derive(Clone, Debug, Default)]
pub struct Line {
    pub time: u64,
    pub text: String,
    pub trans: Option<String>,
    /// 词级时间轴（增强型 LRC 的 `<mm:ss.xx>`）：
    /// (时间, 该标签之后 `text` 里的字节偏移)。空表示只有行级时间。
    pub words: Vec<(u64, usize)>,
}

impl Line {
    /// 这一行唱到 `ms` 时应该填到第几个字节（用于卡拉OK逐字上色）。
    ///
    /// - 有词级标签：在相邻两个标签之间按时间插值，唱到哪填到哪；
    /// - 只有行级时间：在 `[time, end)` 之间线性推进整行。
    ///
    /// 返回的偏移一定落在 UTF-8 字符边界上（否则 Pango 会整行不渲染）。
    pub fn sung_cut(&self, ms: u64, end: u64) -> usize {
        let len = self.text.len();
        if len == 0 {
            return 0;
        }
        if !self.words.is_empty() {
            if ms < self.words[0].0 {
                return 0;
            }
            let k = self.words.partition_point(|(t, _)| *t <= ms) - 1;
            let (t0, o0) = self.words[k];
            let (t1, o1) = match self.words.get(k + 1) {
                Some(&(t, o)) => (t, o),
                None => (end.max(t0 + 1), len),
            };
            let f = if t1 > t0 {
                ((ms - t0) as f64 / (t1 - t0) as f64).clamp(0.0, 1.0)
            } else {
                1.0
            };
            let cut = o0 as f64 + (o1 as f64 - o0 as f64) * f;
            return snap_char(&self.text, cut as usize);
        }
        if ms <= self.time || end <= self.time {
            return 0;
        }
        if ms >= end {
            return len;
        }
        let f = (ms - self.time) as f64 / (end - self.time) as f64;
        snap_char(&self.text, (len as f64 * f) as usize)
    }
}

/// 向下取到合法字符边界，避免把多字节字符切成两半。
fn snap_char(s: &str, mut i: usize) -> usize {
    if i >= s.len() {
        return s.len();
    }
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
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

    /// 第 `i` 行的结束时间：下一行的开始；最后一行按 +6s 兜底。
    pub fn line_end(&self, i: usize) -> u64 {
        match self.lines.get(i + 1) {
            Some(n) => n.time.max(self.lines[i].time + 1),
            None => self.lines[i].time + 6_000,
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

/// 拆出增强型 LRC 的 `<mm:ss.xx>` 词级标签。
///
/// 返回去掉标签的正文，以及每个标签之后正文的字节偏移：
/// `<00:01.00>Hel<00:01.50>lo` → ("Hello", [(1000, 0), (1500, 3)])。
/// 这样正文和偏移一起产出，不会出现「先删标签再按字符数估算」导致的对不上。
fn split_word_tags(s: &str) -> (String, Vec<(u64, usize)>) {
    let mut out = String::with_capacity(s.len());
    let mut words: Vec<(u64, usize)> = Vec::new();
    let mut i = 0;
    while i < s.len() {
        let rest = &s[i..];
        if let Some(after) = rest.strip_prefix('<') {
            if let Some(close) = after.find('>') {
                if let Some(Ok(ms)) = parse_tag(&after[..close]) {
                    words.push((ms, out.len()));
                    i += 1 + close + 1;
                    continue;
                }
            }
        }
        let c = rest.chars().next().unwrap();
        out.push(c);
        i += c.len_utf8();
    }
    (out, words)
}

/// 去掉首尾空白，同时把词级偏移跟着平移（trim 会挪动字节位置）。
fn trim_keep_offsets(s: &str, words: &mut Vec<(u64, usize)>) -> String {
    let t = s.trim();
    if t.is_empty() {
        words.clear();
        return String::new();
    }
    let lead = s.len() - s.trim_start().len();
    for (_, off) in words.iter_mut() {
        *off = off.saturating_sub(lead).min(t.len());
    }
    t.to_string()
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
        let (raw_body, mut words) = split_word_tags(rest);
        let body = trim_keep_offsets(&raw_body, &mut words);
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
                words: words.clone(),
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
                words: Vec::new(),
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
    fn parses_word_level_tags() {
        let l = parse("[00:01.00]<00:01.00>你<00:01.50>好<00:02.00>啊\n[00:03.00]x\n");
        let line = &l.lines[0];
        assert_eq!(line.text, "你好啊");
        assert_eq!(line.words, vec![(1000, 0), (1500, 3), (2000, 6)]);
    }

    #[test]
    fn sung_cut_uses_word_tags_when_present() {
        let l = parse("[00:01.00]<00:01.00>ab<00:02.00>cd\n[00:05.00]next\n");
        let line = &l.lines[0];
        let end = l.line_end(0);
        assert_eq!(line.sung_cut(1_000, end), 0, "第一个词刚开始");
        assert_eq!(line.sung_cut(1_500, end), 1, "两个词之间按时间连续推进");
        assert_eq!(line.sung_cut(2_000, end), 2, "第二个词开始");
        assert_eq!(line.sung_cut(5_000, end), 4, "最后一个词到行尾才唱满");
    }

    #[test]
    fn sung_cut_falls_back_to_line_progress() {
        let l = parse("[00:10.00]abcd\n[00:14.00]efgh\n");
        let line = &l.lines[0];
        let end = l.line_end(0);
        assert_eq!(end, 14_000);
        assert_eq!(line.sung_cut(9_000, end), 0);
        assert_eq!(line.sung_cut(10_000, end), 0);
        assert_eq!(line.sung_cut(12_000, end), 2);
        assert_eq!(line.sung_cut(14_000, end), 4);
    }

    #[test]
    fn sung_cut_never_splits_a_char() {
        // 中文每字 3 字节，按比例算出来的偏移必须落回字符边界
        let l = parse("[00:00.00]一二三四\n[00:04.00]next\n");
        let line = &l.lines[0];
        let end = l.line_end(0);
        for ms in 0..=4_000 {
            let cut = line.sung_cut(ms, end);
            assert!(line.text.is_char_boundary(cut), "ms={ms} cut={cut}");
        }
    }

    #[test]
    fn detects_synced_text() {
        assert!(looks_synced("[ar:x]\n[00:01.00]hi"));
        assert!(!looks_synced("plain\nlyrics"));
    }
}
