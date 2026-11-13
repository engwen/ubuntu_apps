//! Piece table：把「原始文件」与「用户编辑」彻底分开。
//!
//! 这是内存指标的关键。一次编辑不会把原文件读进内存，也不会把被删除的
//! 区间读进内存——被删的 piece 只是从列表里摘下来存进撤销记录，它仍然
//! 指向磁盘上的原始字节（或已经存在的插入缓冲）。因此：
//!
//! ```text
//! 堆内存 ∝ 块缓存(有界) + 用户实际键入的文本 + 撤销记录条数
//! 堆内存与文件大小无关
//! ```
//!
//! 保存采用「写临时文件 + rename」。已打开的原始 fd 仍指向旧 inode，
//! 所以保存之后 piece table、撤销栈、后台搜索快照全部继续有效。

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PieceSource {
    /// 来自打开时的原始文件。
    Original,
    /// 来自 `add_buf`（用户插入的文本）。
    Add,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Piece {
    pub src: PieceSource,
    /// 在对应数据源里的字节偏移。
    pub start: u64,
    pub len: u64,
}

/// 一次编辑的可逆记录。**不保存被删除的内容**。
#[derive(Clone, Debug)]
pub struct UndoRecord {
    pub start: u64,
    /// 编辑发生在 piece 列表的哪个下标。
    pub index: usize,
    /// 被摘下来的 pieces（只是引用，不含字节内容）。
    pub removed: Vec<Piece>,
    /// 新插入的 piece（引用 add_buf 中的一段）。
    pub inserted: Option<Piece>,
}

#[derive(Clone, Debug)]
pub struct PieceTable {
    pub pieces: Vec<Piece>,
    /// `starts[i]` 是 `pieces[i]` 的逻辑起始偏移；末元素为逻辑总长。
    pub starts: Vec<u64>,
    /// 所有用户插入文本的追加缓冲，只增不减（保证重做时引用仍有效）。
    pub add_buf: Vec<u8>,
}

impl PieceTable {
    pub fn new(original_len: u64) -> Self {
        let (pieces, starts) = if original_len == 0 {
            (Vec::new(), vec![0])
        } else {
            (
                vec![Piece {
                    src: PieceSource::Original,
                    start: 0,
                    len: original_len,
                }],
                vec![0, original_len],
            )
        };
        Self {
            pieces,
            starts,
            add_buf: Vec::new(),
        }
    }

    pub fn logical_len(&self) -> u64 {
        *self.starts.last().unwrap_or(&0)
    }

    pub fn add_bytes(&self) -> usize {
        self.add_buf.len()
    }

    /// 找到包含逻辑偏移 `off` 的 piece 下标（`off < logical_len()`）。
    pub fn index_at(&self, off: u64) -> usize {
        self.starts
            .partition_point(|&s| s <= off)
            .saturating_sub(1)
            .min(self.pieces.len().saturating_sub(1).max(0))
    }

    fn rebuild_starts(&mut self) {
        self.starts.clear();
        self.starts.push(0);
        let mut acc = 0u64;
        for p in &self.pieces {
            acc += p.len;
            self.starts.push(acc);
        }
    }

    /// 在逻辑偏移 `logical` 处切一刀，返回切点右侧的 piece 下标。
    fn split_at(&mut self, logical: u64) -> usize {
        let total = self.logical_len();
        if logical >= total {
            return self.pieces.len();
        }
        let i = self.index_at(logical);
        let base = self.starts[i];
        if logical == base {
            return i;
        }
        let p = self.pieces[i];
        let left = logical - base;
        self.pieces[i].len = left;
        self.pieces.insert(
            i + 1,
            Piece {
                src: p.src,
                start: p.start + left,
                len: p.len - left,
            },
        );
        self.rebuild_starts();
        i + 1
    }

    /// 用 `text` 替换逻辑区间 `[start, end)`，返回可撤销/重做的记录。
    pub fn replace(&mut self, start: u64, end: u64, text: &[u8]) -> UndoRecord {
        let total = self.logical_len();
        let start = start.min(total);
        let end = end.min(total).max(start);

        // 必须先切左端再切右端：切左端会插入一个 piece，若先算好右端
        // 下标，它会整体后移一位，导致 drain 区间为空（删除静默失效）。
        // split_at 用的是逻辑偏移，重算 starts 后能找到正确位置。
        let i = self.split_at(start);
        let j = self.split_at(end);
        let removed: Vec<Piece> = self.pieces.drain(i..j).collect();

        let inserted = if text.is_empty() {
            None
        } else {
            let s = self.add_buf.len() as u64;
            self.add_buf.extend_from_slice(text);
            let p = Piece {
                src: PieceSource::Add,
                start: s,
                len: text.len() as u64,
            };
            self.pieces.insert(i, p);
            Some(p)
        };
        self.rebuild_starts();
        UndoRecord {
            start,
            index: i,
            removed,
            inserted,
        }
    }

    pub fn apply_undo(&mut self, rec: &UndoRecord) {
        if let Some(p) = rec.inserted {
            if rec.index < self.pieces.len() && self.pieces[rec.index] == p {
                self.pieces.remove(rec.index);
            }
        }
        if !rec.removed.is_empty() {
            self.pieces
                .splice(rec.index..rec.index, rec.removed.iter().copied());
        }
        self.rebuild_starts();
    }

    pub fn apply_redo(&mut self, rec: &UndoRecord) {
        if !rec.removed.is_empty() {
            let end = (rec.index + rec.removed.len()).min(self.pieces.len());
            self.pieces.drain(rec.index..end);
        }
        if let Some(p) = rec.inserted {
            self.pieces.insert(rec.index.min(self.pieces.len()), p);
        }
        self.rebuild_starts();
    }

    /// 折叠为一个新的 baseline（用于「另存为」或外部重建）。
    pub fn collapse(&mut self, new_len: u64) {
        *self = Self::new(new_len);
    }

    pub fn add_slice(&self, start: u64, len: usize) -> &[u8] {
        let s = start as usize;
        let e = (s + len).min(self.add_buf.len());
        if s >= e {
            &[]
        } else {
            &self.add_buf[s..e]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn original(pt: &PieceTable, logical_off: u64, n: usize) -> Vec<u8> {
        // 仅用于测试：模拟原始文件读取（把 Original piece 的字节当作内容本身）。
        let mut out = vec![0u8; n];
        let mut written = 0;
        let mut i = pt.index_at(logical_off);
        let mut pos = logical_off;
        while written < n && i < pt.pieces.len() {
            let p = pt.pieces[i];
            let within = pos - pt.starts[i];
            let avail = ((p.len - within) as usize).min(n - written);
            for k in 0..avail {
                out[written + k] = match p.src {
                    PieceSource::Original => (p.start + within + k as u64) as u8,
                    PieceSource::Add => pt.add_buf[(p.start + within) as usize + k],
                };
            }
            written += avail;
            pos += avail as u64;
            i += 1;
        }
        out.truncate(written);
        out
    }

    #[test]
    fn insert_then_undo_redo() {
        let mut pt = PieceTable::new(10);
        let rec = pt.replace(3, 5, b"XY");
        assert_eq!(pt.logical_len(), 10);
        assert_eq!(original(&pt, 0, 10), vec![0, 1, 2, b'X', b'Y', 5, 6, 7, 8, 9]);
        pt.apply_undo(&rec);
        assert_eq!(pt.logical_len(), 10);
        assert_eq!(original(&pt, 0, 10), (0u8..10).collect::<Vec<_>>());
        pt.apply_redo(&rec);
        assert_eq!(pt.logical_len(), 10);
        assert_eq!(pt.add_bytes(), 2);
    }

    #[test]
    fn delete_large_range_does_not_buffer_content() {
        let mut pt = PieceTable::new(10_000_000_000);
        // 删除 9 GiB：不应把这段内容读进内存。
        let rec = pt.replace(1_000, 9_000_000_000, b"");
        assert_eq!(pt.add_bytes(), 0, "删除不应分配与区间成比例的内存");
        assert_eq!(pt.logical_len(), 1_000_001_000);
        assert!(rec.removed.len() <= 2, "撤销记录只存 piece 引用");
        pt.apply_undo(&rec);
        assert_eq!(pt.logical_len(), 10_000_000_000, "撤销必须完整恢复");
    }

    #[test]
    fn repeated_insert_undo_restores_length() {
        let mut pt = PieceTable::new(1024);
        for i in 0..200u64 {
            let r = pt.replace(i, i, b"a");
            pt.apply_undo(&r);
        }
        assert_eq!(pt.logical_len(), 1024);
    }
}
