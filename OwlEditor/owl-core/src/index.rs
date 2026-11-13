//! 全文件行索引：后台渐进构建、内存有界、行号与字节偏移可精确互换。
//!
//! ## 为什么需要它
//!
//! 早期版本用「字节偏移 + 估算平均行长」驱动虚拟滚动，滚动条按估算值
//! 换算，导致：滚动时同一行反复出现、点击滚动条换算回去又跳回原处、
//! 行号只能显示 `~估算值`。这些都是用户能直接看到的错误。
//!
//! 这里改为**行号驱动**：打开文件时首屏立即显示（不等待任何扫描），同时
//! 后台线程流式扫描一遍文件，每隔 [`INDEX_STRIDE`] 记一个「行首锚点」
//! `(byte, line)`。10 GiB 文件约 16 万个锚点、约 2.6 MiB 内存，远低于
//! 200 MiB 预算；扫描期间 UI 完全不阻塞，进度显示在状态栏。
//!
//! 锚点保证落在行首，因此 `byte_of_line` 只需从最近锚点局部扫描不超过
//! 一个 stride 就能得到精确行首，`line_of_byte` 同理。
//!
//! 编辑时走 [`LineIndex::apply_edit`] 做增量平移，不必重扫全文件。

use crate::ByteReader;

/// 相邻锚点的最小字节间距。
pub const INDEX_STRIDE: u64 = 64 * 1024;
/// 后台每批扫描的字节数。
pub const INDEX_BATCH: u64 = 16 * 1024 * 1024;
/// 局部扫描缓冲区。
const SCAN_BUF: usize = 64 * 1024;

#[derive(Debug, Clone)]
pub struct LineIndex {
    /// `(行首字节偏移, 0 基行号)`，严格递增，首项恒为 `(0, 0)`。
    anchors: Vec<(u64, u64)>,
    total_bytes: u64,
    /// 编辑器语义下的总行数：换行数 + 1（末尾换行后仍有一个可编辑空行）。
    total_lines: u64,
    pub complete: bool,
    pub scanned: u64,
}

impl LineIndex {
    pub fn empty(total_bytes: u64) -> Self {
        Self {
            anchors: vec![(0, 0)],
            total_bytes,
            total_lines: 1,
            complete: false,
            scanned: 0,
        }
    }

    pub fn total_lines(&self) -> u64 {
        self.total_lines.max(1)
    }

    pub fn total_bytes(&self) -> u64 {
        self.total_bytes
    }

    pub fn anchor_count(&self) -> usize {
        self.anchors.len()
    }

    pub fn is_complete(&self) -> bool {
        self.complete
    }

    pub fn progress(&self) -> f64 {
        if self.total_bytes == 0 {
            1.0
        } else {
            (self.scanned as f64 / self.total_bytes as f64).min(1.0)
        }
    }

    /// `<= byte` 的最近锚点（返回值是 Copy，调用方可立即释放借用）。
    pub fn anchor_for_byte(&self, byte: u64) -> (u64, u64) {
        let i = match self.anchors.binary_search_by_key(&byte, |&(b, _)| b) {
            Ok(i) => i,
            Err(i) => i.saturating_sub(1),
        };
        self.anchors[i]
    }

    /// `<= line` 的最近锚点。
    pub fn anchor_for_line(&self, line: u64) -> (u64, u64) {
        let i = match self.anchors.binary_search_by_key(&line, |&(_, l)| l) {
            Ok(i) => i,
            Err(i) => i.saturating_sub(1),
        };
        self.anchors[i]
    }

    /// 精确求第 `line` 行的行首字节偏移。索引未建完时返回估算值。
    pub fn byte_of_line<R: ByteReader>(&self, r: &mut R, line: u64) -> u64 {
        if line == 0 {
            return 0;
        }
        if !self.complete {
            return self.estimated_byte_of_line(line);
        }
        let (b, l) = self.anchor_for_line(line.min(self.total_lines() - 1));
        let mut need = line.saturating_sub(l);
        if need == 0 {
            return b;
        }
        let mut pos = b;
        let mut buf = vec![0u8; SCAN_BUF];
        while need > 0 && pos < r.len() {
            let n = r.read_at(pos, &mut buf);
            if n == 0 {
                break;
            }
            for k in memchr::memchr_iter(b'\n', &buf[..n]) {
                need -= 1;
                if need == 0 {
                    return pos + k as u64 + 1;
                }
            }
            pos += n as u64;
        }
        pos.min(r.len())
    }

    /// 精确求字节偏移所在行。索引未建完时返回估算值。
    pub fn line_of_byte<R: ByteReader>(&self, r: &mut R, byte: u64) -> u64 {
        if !self.complete {
            return self.estimated_line_of_byte(byte);
        }
        let (b, l) = self.anchor_for_byte(byte);
        if byte <= b {
            return l;
        }
        let mut pos = b;
        let mut count = 0u64;
        let mut buf = vec![0u8; SCAN_BUF];
        let target = byte.min(r.len());
        while pos < target {
            let want = (target - pos).min(buf.len() as u64) as usize;
            let n = r.read_at(pos, &mut buf[..want]);
            if n == 0 {
                break;
            }
            count += memchr::memchr_iter(b'\n', &buf[..n]).count() as u64;
            pos += n as u64;
        }
        l + count
    }

    /// 索引构建中使用的估算（只用于临时显示，完成后全部变精确）。
    pub fn estimated_byte_of_line(&self, line: u64) -> u64 {
        if self.scanned == 0 || self.total_lines <= 1 {
            return (line * 80).min(self.total_bytes);
        }
        let avg = (self.scanned as f64 / self.total_lines as f64).max(1.0);
        ((line as f64 * avg) as u64).min(self.total_bytes)
    }

    pub fn estimated_line_of_byte(&self, byte: u64) -> u64 {
        if self.scanned == 0 || self.total_lines <= 1 {
            return byte / 80;
        }
        let avg = (self.scanned as f64 / self.total_lines as f64).max(1.0);
        (byte as f64 / avg) as u64
    }

    /// 一次编辑后增量维护索引：把编辑点之后的锚点整体平移，删除落在
    /// 被替换区间内的锚点。复杂度 O(锚点数)，与文件大小无关。
    pub fn apply_edit(
        &mut self,
        start: u64,
        old_end: u64,
        new_end: u64,
        old_newlines: u64,
        new_newlines: u64,
    ) {
        let db = new_end as i64 - old_end as i64;
        let dl = new_newlines as i64 - old_newlines as i64;
        self.anchors.retain(|(b, _)| *b <= start || *b > old_end);
        for (b, l) in self.anchors.iter_mut() {
            if *b > old_end {
                *b = (*b as i64 + db).max(0) as u64;
                *l = (*l as i64 + dl).max(0) as u64;
            }
        }
        self.total_bytes = (self.total_bytes as i64 + db).max(0) as u64;
        self.total_lines = (self.total_lines as i64 + dl).max(1) as u64;
        self.scanned = self.total_bytes;
        // 锚点可能因为删除而不再严格递增（同一行首），去重重排。
        self.anchors.sort_by_key(|&(b, _)| b);
        self.anchors.dedup_by_key(|&mut (b, _)| b);
    }
}

/// 后台线程持有：分批推进，随时可放弃。
pub struct IndexBuilder<R: ByteReader> {
    r: R,
    total_bytes: u64,
    cursor: u64,
    newlines: u64,
    next_anchor: u64,
    anchors: Vec<(u64, u64)>,
}

impl<R: ByteReader> IndexBuilder<R> {
    pub fn new(r: R) -> Self {
        let total_bytes = r.len();
        Self {
            r,
            total_bytes,
            cursor: 0,
            newlines: 0,
            next_anchor: INDEX_STRIDE,
            anchors: vec![(0, 0)],
        }
    }

    /// 处理至多 `batch` 字节，返回是否已完成。
    pub fn step(&mut self, batch: u64) -> bool {
        let end = (self.cursor + batch).min(self.total_bytes);
        let mut buf = vec![0u8; SCAN_BUF];
        while self.cursor < end {
            let want = (end - self.cursor).min(buf.len() as u64) as usize;
            let n = self.r.read_at(self.cursor, &mut buf[..want]);
            if n == 0 {
                self.cursor = self.total_bytes;
                break;
            }
            // memchr 用 SIMD 找换行，10 GiB 扫描比逐字节快一个数量级。
            for k in memchr::memchr_iter(b'\n', &buf[..n]) {
                self.newlines += 1;
                let after = self.cursor + k as u64 + 1;
                if after >= self.next_anchor {
                    self.anchors.push((after, self.newlines));
                    self.next_anchor = after + INDEX_STRIDE;
                }
            }
            self.cursor += n as u64;
        }
        self.cursor >= self.total_bytes
    }

    pub fn progress(&self) -> f64 {
        if self.total_bytes == 0 {
            1.0
        } else {
            (self.cursor as f64 / self.total_bytes as f64).min(1.0)
        }
    }

    /// 构建中的部分索引，可用于状态栏进度与近似行号。
    pub fn partial(&self) -> LineIndex {
        LineIndex {
            anchors: self.anchors.clone(),
            total_bytes: self.total_bytes,
            total_lines: self.newlines + 1,
            complete: false,
            scanned: self.cursor,
        }
    }

    pub fn finish(self) -> LineIndex {
        LineIndex {
            anchors: self.anchors,
            total_bytes: self.total_bytes,
            total_lines: self.newlines + 1,
            complete: true,
            scanned: self.total_bytes,
        }
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

    fn build(data: &[u8]) -> (LineIndex, Mem) {
        let mut b = IndexBuilder::new(Mem(data.to_vec()));
        while !b.step(INDEX_BATCH) {}
        (b.finish(), Mem(data.to_vec()))
    }

    #[test]
    fn exact_line_byte_mapping() {
        let data = b"one\ntwo\nthree\nfour\n";
        let (idx, mut r) = build(data);
        assert_eq!(idx.total_lines(), 5, "末尾换行后应有一个可编辑空行");
        let starts: Vec<u64> = (0..5).map(|l| idx.byte_of_line(&mut r, l)).collect();
        assert_eq!(starts, vec![0, 4, 8, 14, 19]);
        for (line, &b) in starts.iter().enumerate() {
            assert_eq!(idx.line_of_byte(&mut r, b), line as u64);
        }
    }

    #[test]
    fn anchors_are_bounded_for_large_files() {
        // 4 MiB 数据，每 80 字节一行；锚点数量应远小于行数。
        let mut data = vec![b'x'; 4 * 1024 * 1024];
        for i in (0..data.len()).step_by(80) {
            data[i] = b'\n';
        }
        let (idx, mut r) = build(&data);
        assert!(idx.anchor_count() <= 4 * 1024 * 1024 / INDEX_STRIDE as usize + 4);
        let last = idx.total_lines() - 1;
        let b = idx.byte_of_line(&mut r, last);
        assert!(b <= data.len() as u64);
        assert_eq!(idx.line_of_byte(&mut r, b), last);
    }

    #[test]
    fn edit_shifts_anchors_without_full_rescan() {
        // 构造一份足以产生多个锚点的数据，验证编辑后锚点整体平移。
        let mut data = vec![b'x'; 400 * 1024];
        for i in (80..data.len()).step_by(80) {
            data[i - 1] = b'\n';
        }
        let (mut idx, _) = build(&data);
        assert!(idx.anchor_count() > 2);
        let before_lines = idx.total_lines();
        let (_, before_line) = idx.anchor_for_byte(200 * 1024);

        // 在 offset 10 处插入 8 字节、带来 2 个换行。
        idx.apply_edit(10, 10, 18, 0, 2);
        assert_eq!(idx.total_lines(), before_lines + 2);
        assert_eq!(idx.total_bytes(), data.len() as u64 + 8);
        let (_, after_line) = idx.anchor_for_byte(200 * 1024 + 8);
        assert!(after_line >= before_line + 2, "编辑点之后的锚点行号必须平移");
    }
}
