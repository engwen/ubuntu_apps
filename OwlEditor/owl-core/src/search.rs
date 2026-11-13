//! 流式全文查找：固定窗口 + 重叠区，内存恒定、可随时取消。
//!
//! Notepad-- 的「全文搜索把 UI 线程挂住」和正则回溯是两个典型坑。
//! 这里只做字面量（可选 ASCII 大小写不敏感），用 `memchr::memmem`
//! 的 SIMD 子串搜索，窗口之间保留 `needle.len()-1` 字节重叠以处理
//! 跨块命中；结果通过回调增量吐出，天然适配后台线程 + 进度回传。

use crate::ByteReader;
use memchr::memmem::Finder;
use std::sync::atomic::{AtomicBool, Ordering};

pub const SEARCH_WINDOW: usize = 64 * 1024;

#[derive(Debug, Clone)]
pub struct SearchQuery {
    pub needle: Vec<u8>,
    pub case_insensitive: bool,
}

impl SearchQuery {
    pub fn new(needle: impl Into<Vec<u8>>) -> Self {
        Self {
            needle: needle.into(),
            case_insensitive: false,
        }
    }
    pub fn case_insensitive(mut self, yes: bool) -> Self {
        self.case_insensitive = yes;
        self
    }
    pub fn is_empty(&self) -> bool {
        self.needle.is_empty()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hit {
    pub start: u64,
    pub end: u64,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SearchOutcome {
    pub scanned: u64,
    pub hits: usize,
    pub cancelled: bool,
}

fn ascii_lower(b: u8) -> u8 {
    if b.is_ascii_uppercase() {
        b + 32
    } else {
        b
    }
}

/// 从 `from` 开始向后查找，命中通过 `emit` 增量返回。
///
/// * `cancel` 置位后立即停止（用于用户改动查询词 / 关闭窗口）。
/// * `max_hits` 限制收集数量，避免一次查找在内存里堆出与文件大小成比例的结果。
pub fn search<R, F>(
    r: &mut R,
    q: &SearchQuery,
    from: u64,
    max_hits: usize,
    cancel: &AtomicBool,
    mut emit: F,
) -> SearchOutcome
where
    R: ByteReader,
    F: FnMut(Hit),
{
    let mut out = SearchOutcome::default();
    if q.is_empty() {
        return out;
    }
    let needle: Vec<u8> = if q.case_insensitive {
        q.needle.iter().map(|&b| ascii_lower(b)).collect()
    } else {
        q.needle.clone()
    };
    let finder = Finder::new(&needle).into_owned();
    let overlap = needle.len() - 1;
    let window = SEARCH_WINDOW.max(needle.len() * 2);
    let step = window - overlap;
    let mut pos = from;
    let mut buf = vec![0u8; window];

    loop {
        if cancel.load(Ordering::Relaxed) {
            out.cancelled = true;
            break;
        }
        let n = r.read_at(pos, &mut buf[..window]);
        if n == 0 {
            break;
        }
        out.scanned += n as u64;

        if q.case_insensitive {
            let folded: Vec<u8> = buf[..n].iter().map(|&b| ascii_lower(b)).collect();
            scan_window(&finder, &folded, pos, step as u64, needle.len(), &mut emit, &mut out, max_hits);
        } else {
            scan_window(&finder, &buf[..n], pos, step as u64, needle.len(), &mut emit, &mut out, max_hits);
        }
        if out.hits >= max_hits {
            break;
        }
        if n < step || pos + n as u64 >= r.len() {
            break;
        }
        pos += step as u64;
    }
    out
}

fn scan_window<F: FnMut(Hit)>(
    finder: &Finder<'_>,
    data: &[u8],
    pos: u64,
    step: u64,
    need: usize,
    emit: &mut F,
    out: &mut SearchOutcome,
    max_hits: usize,
) {
    for idx in finder.find_iter(data) {
        let s = pos + idx as u64;
        // 起点只在本窗口「独属区」上报，跨窗口命中不会重复。
        if s >= pos + step {
            continue;
        }
        if idx + need > data.len() {
            continue;
        }
        emit(Hit {
            start: s,
            end: s + need as u64,
        });
        out.hits += 1;
        if out.hits >= max_hits {
            return;
        }
    }
}

/// 向前查找（查找上一个）：从 `before` 之前的最多 `span` 字节里找最后一个命中。
pub fn search_backward<R: ByteReader>(
    r: &mut R,
    q: &SearchQuery,
    before: u64,
    span: u64,
    cancel: &AtomicBool,
) -> Option<Hit> {
    if q.is_empty() || before == 0 {
        return None;
    }
    let from = before.saturating_sub(span);
    let mut hits = Vec::new();
    let outcome = search(r, q, from, usize::MAX, cancel, |h| {
        if h.start < before {
            hits.push(h);
            if hits.len() > 4096 {
                hits.remove(0);
            }
        }
    });
    let _ = outcome;
    hits.into_iter().last()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    struct Mem(Vec<u8>);
    impl ByteReader for Mem {
        fn len(&self) -> u64 {
            self.0.len() as u64
        }
        fn read_at(&mut self, off: u64, buf: &mut [u8]) -> usize {
            let o = off as usize;
            if o >= self.0.len() {
                return 0;
            }
            let n = buf.len().min(self.0.len() - o);
            buf[..n].copy_from_slice(&self.0[o..o + n]);
            n
        }
    }

    fn collect(data: &[u8], q: &SearchQuery, from: u64) -> Vec<u64> {
        let mut r = Mem(data.to_vec());
        let mut v = Vec::new();
        search(&mut r, q, from, 10_000, &AtomicBool::new(false), |h| {
            v.push(h.start)
        });
        v
    }

    #[test]
    fn finds_all_occurrences() {
        let d = b"hello world\nhello owl\nhello end\n".to_vec();
        let q = SearchQuery::new(b"hello".to_vec());
        assert_eq!(collect(&d, &q, 0), vec![0, 12, 22]);
    }

    #[test]
    fn hit_straddling_window_boundary_is_not_lost_or_duplicated() {
        let mut d = vec![b'x'; SEARCH_WINDOW * 3];
        let at = SEARCH_WINDOW - 3;
        d[at..at + 6].copy_from_slice(b"ABCDEF");
        let q = SearchQuery::new(b"ABCDEF".to_vec());
        let hits = collect(&d, &q, 0);
        assert_eq!(hits, vec![at as u64]);
    }

    #[test]
    fn case_insensitive_and_backward() {
        let d = b"Foo foo FOO\n".to_vec();
        let q = SearchQuery::new(b"foo".to_vec()).case_insensitive(true);
        assert_eq!(collect(&d, &q, 0), vec![0, 4, 8]);
        let mut r = Mem(d.clone());
        let last = search_backward(&mut r, &q, 8, 100, &AtomicBool::new(false));
        assert_eq!(last.unwrap().start, 4);
    }

    #[test]
    fn cancel_stops_early() {
        let d = vec![b'a'; 8 * 1024 * 1024];
        let mut r = Mem(d);
        let cancel = AtomicBool::new(true);
        let out = search(&mut r, &SearchQuery::new(b"x".to_vec()), 0, 100, &cancel, |_| {});
        assert!(out.cancelled);
        assert_eq!(out.scanned, 0);
    }
}
