//! 惰性行索引：**不一次性扫描全文件换行**。
//!
//! Crane 会在后台把整份文件的行索引建完；OwlEditor 按需求更严格——
//! 只在「用户要看的区域」按需解析。做法是维护一张**有上限**的稀疏锚点表：
//!
//! ```text
//! anchors: BTreeMap<字节偏移, 该偏移之前共有多少个 \n>
//! ```
//!
//! * 打开文件时只有 `(0, 0)` 一个锚点，只解析第一屏。
//! * 需要显示第 N 屏时，从 <= 目标偏移的最近锚点开始向前数换行，
//!   顺路每隔一块记一个新锚点。
//! * 锚点数量超过 [`MAX_ANCHORS`] 时按步长稀释，内存恒定。
//! * 若目标离最近锚点太远（用户直接拖到 90% 处），**不阻塞 UI**：
//!   先用已观测行密度给出估算行号（界面加 `~` 标记），同时交给后台
//!   线程流式精修，完成后落一个精确锚点并刷新。
//!
//! 因此「扫描量」只与用户实际浏览过的区域成正比，与文件体积无关。

use crate::ByteReader;
use std::collections::BTreeMap;

/// 锚点表上限：4096 × (8+8) 字节 ≈ 64 KiB，与文件大小无关。
pub const MAX_ANCHORS: usize = 4096;
/// 同步精确扫描的最大跨度（超过则走后台精修）。
pub const MAX_SYNC_SCAN: u64 = 2 * 1024 * 1024;
/// 扫描时每隔多少字节记一个锚点。
pub const ANCHOR_STRIDE: u64 = 64 * 1024;
/// 后台精修每批处理的字节数。
pub const REFINE_BATCH: u64 = 8 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineResolution {
    /// 精确行号（0 基）。
    Exact(u64),
    /// 估算行号；附带后台精修需要扫描的区间起点。
    Estimated { line: u64, refine_from: u64 },
}

impl LineResolution {
    pub fn line(&self) -> u64 {
        match *self {
            LineResolution::Exact(l) => l,
            LineResolution::Estimated { line, .. } => line,
        }
    }
    pub fn is_exact(&self) -> bool {
        matches!(self, LineResolution::Exact(_))
    }
}

#[derive(Debug, Clone)]
pub struct LazyLineIndex {
    anchors: BTreeMap<u64, u64>,
    /// 累计按需扫描的字节数（含后台精修），用于验证「没有全量扫描」。
    pub scanned_bytes: u64,
    /// 同步解析次数。
    pub resolve_calls: u64,
    /// 后台精修次数。
    pub refinements: u64,
}

impl Default for LazyLineIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl LazyLineIndex {
    pub fn new() -> Self {
        let mut anchors = BTreeMap::new();
        anchors.insert(0u64, 0u64);
        Self {
            anchors,
            scanned_bytes: 0,
            resolve_calls: 0,
            refinements: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.anchors.len()
    }

    pub fn is_empty(&self) -> bool {
        self.anchors.len() <= 1
    }

    /// 返回 <= `byte` 的最近锚点。
    pub fn anchor_before(&self, byte: u64) -> (u64, u64) {
        self.anchors
            .range(..=byte)
            .next_back()
            .map(|(&b, &l)| (b, l))
            .unwrap_or((0, 0))
    }

    /// 用已观测到的最大锚点做密度外推。
    pub fn estimate(&self, byte: u64) -> u64 {
        let (b, l) = self
            .anchors
            .iter()
            .next_back()
            .map(|(&b, &l)| (b, l))
            .unwrap_or((0, 0));
        if b == 0 || l == 0 {
            return byte / 64;
        }
        ((byte as f64) * (l as f64) / (b as f64)) as u64
    }

    /// 解析某个逻辑字节偏移的行号。
    ///
    /// 若最近锚点在 [`MAX_SYNC_SCAN`] 内，同步流式数换行得到精确值；
    /// 否则立即返回估算值 + 精修起点，供调用方起后台任务。
    pub fn resolve<R: ByteReader>(&mut self, r: &mut R, byte: u64) -> LineResolution {
        self.resolve_calls += 1;
        let (ab, al) = self.anchor_before(byte);
        let dist = byte.saturating_sub(ab);
        if dist == 0 {
            return LineResolution::Exact(al);
        }
        if dist > MAX_SYNC_SCAN {
            return LineResolution::Estimated {
                line: self.estimate(byte),
                refine_from: ab,
            };
        }
        let nl = self.scan_count(r, ab, byte, al, true);
        LineResolution::Exact(al + nl)
    }

    /// 从 `from` 数到 `to` 的换行数；`record` 为真时顺路写锚点。
    pub fn scan_count<R: ByteReader>(
        &mut self,
        r: &mut R,
        from: u64,
        to: u64,
        from_line: u64,
        record: bool,
    ) -> u64 {
        let mut buf = vec![0u8; 64 * 1024];
        let mut pos = from;
        let mut count = 0u64;
        let mut next_anchor = from + ANCHOR_STRIDE;
        while pos < to {
            let want = (to - pos).min(buf.len() as u64) as usize;
            let n = r.read_at(pos, &mut buf[..want]);
            if n == 0 {
                break;
            }
            for &b in &buf[..n] {
                if b == b'\n' {
                    count += 1;
                }
            }
            pos += n as u64;
            self.scanned_bytes += n as u64;
            if record && pos >= next_anchor {
                self.anchors.insert(pos, from_line + count);
                next_anchor = next_anchor.saturating_add(ANCHOR_STRIDE);
            }
        }
        if record {
            self.anchors.insert(to, from_line + count);
            self.dilute();
        }
        count
    }

    /// 后台精修完成后落一个精确锚点。
    pub fn record_refinement(&mut self, byte: u64, line: u64) {
        self.refinements += 1;
        self.anchors.insert(byte, line);
        self.dilute();
    }

    /// 编辑改变了 `byte` 之后的逻辑偏移，丢弃其后的锚点。
    ///
    /// 只删除、不插入新锚点：`<= byte` 的锚点行号仍然正确，后续
    /// `resolve` 会从它重新按需扫描，自然覆盖编辑造成的变化。
    pub fn invalidate_after(&mut self, byte: u64) {
        let stale: Vec<u64> = self.anchors.range(byte.saturating_add(1)..).map(|(&b, _)| b).collect();
        for b in stale {
            self.anchors.remove(&b);
        }
    }

    /// 稀释到上限以内：保留偶数序号锚点，始终保留首尾。
    fn dilute(&mut self) {
        if self.anchors.len() <= MAX_ANCHORS {
            return;
        }
        let all: Vec<(u64, u64)> = self.anchors.iter().map(|(&b, &l)| (b, l)).collect();
        let last = all.len() - 1;
        let keep: Vec<(u64, u64)> = all
            .into_iter()
            .enumerate()
            .filter(|(i, _)| i % 2 == 0 || *i == last)
            .map(|(_, v)| v)
            .collect();
        self.anchors = keep.into_iter().collect();
        self.anchors.entry(0).or_insert(0);
    }
}

/// 供后台线程使用的精修作业：只数换行，不碰 UI 侧的锚点表。
#[derive(Debug, Clone)]
pub struct RefineJob {
    pub from: u64,
    pub to: u64,
    pub from_line: u64,
    pub cursor: u64,
    pub newlines: u64,
}

impl RefineJob {
    pub fn new(from: u64, to: u64, from_line: u64) -> Self {
        Self {
            from,
            to,
            from_line,
            cursor: from,
            newlines: 0,
        }
    }

    /// 处理一批；返回 `Some(精确行数)` 表示完成。
    pub fn step<R: ByteReader>(&mut self, r: &mut R, batch: u64) -> Option<u64> {
        let end = (self.cursor + batch).min(self.to);
        let mut buf = vec![0u8; 64 * 1024];
        while self.cursor < end {
            let want = (end - self.cursor).min(buf.len() as u64) as usize;
            let n = r.read_at(self.cursor, &mut buf[..want]);
            if n == 0 {
                self.cursor = self.to;
                break;
            }
            for &b in &buf[..n] {
                if b == b'\n' {
                    self.newlines += 1;
                }
            }
            self.cursor += n as u64;
        }
        if self.cursor >= self.to {
            Some(self.from_line + self.newlines)
        } else {
            None
        }
    }

    pub fn progress(&self) -> f64 {
        let total = self.to.saturating_sub(self.from);
        if total == 0 {
            return 1.0;
        }
        (self.cursor - self.from) as f64 / total as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn opening_does_not_scan_whole_file() {
        let mut data = vec![b'x'; 8 * 1024 * 1024];
        for i in (0..data.len()).step_by(80) {
            data[i] = b'\n';
        }
        let mut r = Mem(data);
        let mut idx = LazyLineIndex::new();
        // 只解析第一屏（4 KiB）
        let res = idx.resolve(&mut r, 4096);
        assert!(res.is_exact());
        assert!(
            idx.scanned_bytes <= 4096,
            "打开只应扫描视口大小，实际 {} 字节",
            idx.scanned_bytes
        );
    }

    #[test]
    fn far_jump_is_estimated_then_refined() {
        let mut data = vec![b'x'; 16 * 1024 * 1024];
        for i in (0..data.len()).step_by(100) {
            data[i] = b'\n';
        }
        let mut r = Mem(data.clone());
        let mut idx = LazyLineIndex::new();
        let target = 12 * 1024 * 1024;
        let res = idx.resolve(&mut r, target);
        assert!(matches!(res, LineResolution::Estimated { .. }));
        assert_eq!(idx.scanned_bytes, 0, "远距离跳转不应同步扫描");

        let expected = (0..target).filter(|&i| data[i as usize] == b'\n').count() as u64;
        let (ab, al) = idx.anchor_before(target);
        let mut job = RefineJob::new(ab, target, al);
        let mut r2 = Mem(data);
        let exact = loop {
            if let Some(v) = job.step(&mut r2, REFINE_BATCH) {
                break v;
            }
        };
        idx.record_refinement(target, exact);
        assert_eq!(idx.resolve(&mut r2, target), LineResolution::Exact(exact));
        assert_eq!(exact, expected);
    }

    #[test]
    fn anchors_stay_bounded() {
        let mut r = Mem(vec![b'a'; 32 * 1024 * 1024]);
        let mut idx = LazyLineIndex::new();
        // 模拟连续浏览大量区域：每次解析都会新增锚点
        for i in 1..6000u64 {
            let byte = i * 4096;
            if byte >= r.len() {
                break;
            }
            let (ab, al) = idx.anchor_before(byte);
            idx.scan_count(&mut r, ab, byte, al, true);
        }
        assert!(idx.len() <= MAX_ANCHORS, "锚点 {} 超过上限", idx.len());
    }

    #[test]
    fn edit_invalidates_later_anchors() {
        let mut idx = LazyLineIndex::new();
        idx.record_refinement(1000, 10);
        idx.record_refinement(2000, 20);
        idx.record_refinement(3000, 30);
        idx.invalidate_after(1500);
        assert!(idx.anchor_before(3000).0 <= 1500);
        assert_eq!(idx.len() <= 2, true);
    }
}
