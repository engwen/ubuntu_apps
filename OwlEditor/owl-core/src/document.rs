//! 文档门面：把分块读取、惰性行索引、piece table、撤销与保存拼在一起。

use crate::chunks::ChunkSource;
use crate::lines::{LazyLineIndex, LineResolution};
use crate::pieces::{PieceSource, PieceTable, UndoRecord};
use crate::utf8;
use crate::ByteReader;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// 一次视口渲染最多读取的字节数。与文件大小无关。
pub const MAX_VIEW_BYTES: usize = 256 * 1024;

/// 撤销/重做前后被替换区间的长度快照（字节偏移 + 长度，不含内容）。
///
/// `old_len` 是**变更前** `[start, start+old_len)` 的长度，`new_len` 是
/// **变更后** 该位置的新内容长度。UI 层据此在两个时点数换行，复用
/// `apply_index_edit()` 做增量索引维护，避免全量重建。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EditSpan {
    pub start: u64,
    pub old_len: u64,
    pub new_len: u64,
}

fn edit_span(rec: &UndoRecord, undo: bool) -> EditSpan {
    let inserted = rec.inserted.map(|p| p.len).unwrap_or(0);
    let removed: u64 = rec.removed.iter().map(|p| p.len).sum();
    if undo {
        // 撤销：删掉当初插入的内容，恢复被删的内容。
        EditSpan { start: rec.start, old_len: inserted, new_len: removed }
    } else {
        // 重做：删掉被恢复的内容，重新插入当初的文本。
        EditSpan { start: rec.start, old_len: removed, new_len: inserted }
    }
}

#[derive(Debug, Clone)]
pub struct RenderLine {
    /// 0 基行号。
    pub number: u64,
    /// 该行第一个字节的逻辑偏移。
    pub byte_start: u64,
    /// 行内容的字节长度（不含换行）。
    pub byte_len: u64,
    /// 已解码的 UTF-8 文本。
    pub text: String,
    /// 该行是否严格合法的 UTF-8。
    pub valid_utf8: bool,
    /// 行尾是否因视口缓冲截断（超长行）。
    pub truncated: bool,
    /// 行号是否还是估算值（后台精修中）。
    pub number_estimated: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct Stats {
    pub logical_len: u64,
    pub original_len: u64,
    pub cached_bytes: usize,
    pub add_bytes: usize,
    pub anchors: usize,
    /// 累计按需扫描的字节数。打开 10 GiB 文件后应远小于文件长度。
    pub scanned_bytes: u64,
    pub resolve_calls: u64,
    pub refinements: u64,
    pub undo_depth: usize,
    pub redo_depth: usize,
    pub dirty: bool,
    /// 与文件内容相关的堆内存上界（不含 GTK/Pango 的界面对象）。
    pub heap_upper_bound: usize,
}

/// 供后台线程独立读取的不可变快照。
///
/// 它复制的是 piece table 的引用列表与一个独立的原始文件句柄，
/// 因此不含任何与文件大小成比例的数据。
pub struct Snapshot {
    source: ChunkSource,
    pieces: PieceTable,
}

impl Snapshot {
    pub fn len(&self) -> u64 {
        self.pieces.logical_len()
    }
}

impl ByteReader for Snapshot {
    fn len(&self) -> u64 {
        self.pieces.logical_len()
    }
    fn read_at(&mut self, off: u64, buf: &mut [u8]) -> usize {
        read_pieces(&self.pieces, &mut self.source, off, buf)
    }
}

/// 把逻辑偏移映射到 piece 序列并读取。整个 owl-core 只有这一处
/// 真正把「逻辑文档」翻译成「磁盘字节 + 编辑缓冲」。
fn read_pieces(
    pt: &PieceTable,
    source: &mut ChunkSource,
    off: u64,
    buf: &mut [u8],
) -> usize {
    let total = pt.logical_len();
    if buf.is_empty() || off >= total {
        return 0;
    }
    let mut i = pt.index_at(off);
    let mut pos = off;
    let mut written = 0usize;
    while written < buf.len() && i < pt.pieces.len() {
        let p = pt.pieces[i];
        let within = pos - pt.starts[i];
        let avail = ((p.len - within) as usize).min(buf.len() - written);
        if avail == 0 {
            break;
        }
        let dst = &mut buf[written..written + avail];
        let got = match p.src {
            PieceSource::Original => source.read_at(p.start + within, dst).unwrap_or(0),
            PieceSource::Add => {
                let s = (p.start + within) as usize;
                let e = (s + avail).min(pt.add_buf.len());
                if s < e {
                    dst[..(e - s)].copy_from_slice(&pt.add_buf[s..e]);
                    e - s
                } else {
                    0
                }
            }
        };
        written += got;
        pos += got as u64;
        if got < avail {
            break;
        }
        i += 1;
    }
    written
}

struct LogicalReader<'a> {
    source: &'a mut ChunkSource,
    pt: &'a PieceTable,
}

impl ByteReader for LogicalReader<'_> {
    fn len(&self) -> u64 {
        self.pt.logical_len()
    }
    fn read_at(&mut self, off: u64, buf: &mut [u8]) -> usize {
        read_pieces(self.pt, self.source, off, buf)
    }
}

pub struct Document {
    pub path: PathBuf,
    source: ChunkSource,
    pieces: PieceTable,
    lines: LazyLineIndex,
    undo: Vec<UndoRecord>,
    redo: Vec<UndoRecord>,
    pub dirty: bool,
}

impl Document {
    /// 打开任意大小的文件。**不扫描换行、不建立全量索引、不读入全文**。
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        let source = ChunkSource::open(&path)?;
        let pieces = PieceTable::new(source.len());
        Ok(Self {
            path,
            source,
            pieces,
            lines: LazyLineIndex::new(),
            undo: Vec::new(),
            redo: Vec::new(),
            dirty: false,
        })
    }

    /// 新建一个空文档（先落一个 0 字节文件，保持读取路径一致）。
    pub fn empty(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        File::create(&path)?;
        let source = ChunkSource::open(&path)?;
        Ok(Self {
            path,
            source,
            pieces: PieceTable::new(0),
            lines: LazyLineIndex::new(),
            undo: Vec::new(),
            redo: Vec::new(),
            dirty: false,
        })
    }

    /// 全新的未命名缓冲区：**不落任何可见文件**，`path` 为空。
    ///
    /// 用于「没有打开文件也能直接打字」：内容全部活在 piece table 里，
    /// 第一次保存时再由 UI 层询问文件名与位置（`save_to`）。
    pub fn untitled() -> io::Result<Self> {
        Ok(Self {
            path: PathBuf::new(),
            source: ChunkSource::anonymous()?,
            pieces: PieceTable::new(0),
            lines: LazyLineIndex::new(),
            undo: Vec::new(),
            redo: Vec::new(),
            dirty: false,
        })
    }

    /// 是否还没有落盘路径（未命名缓冲区）。
    pub fn is_untitled(&self) -> bool {
        self.path.as_os_str().is_empty()
    }

    pub fn len(&self) -> u64 {
        self.pieces.logical_len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn read_at(&mut self, off: u64, buf: &mut [u8]) -> usize {
        read_pieces(&self.pieces, &mut self.source, off, buf)
    }

    pub fn read_range(&mut self, off: u64, len: usize) -> Vec<u8> {
        let mut out = vec![0u8; len];
        let n = self.read_at(off, &mut out);
        out.truncate(n);
        out
    }

    /// 从 `top_byte` 往回找到所在行的行首（超长行最多回退 1 MiB）。
    fn line_start_before(&mut self, byte: u64) -> u64 {
        if byte == 0 {
            return 0;
        }
        let mut back = 4096u64;
        loop {
            let from = byte.saturating_sub(back);
            let mut buf = vec![0u8; (byte - from) as usize];
            let n = self.read_at(from, &mut buf);
            if n == 0 {
                return from;
            }
            if let Some(i) = utf8::last_newline(&buf[..n]) {
                return from + i as u64 + 1;
            }
            if from == 0 || back >= 1 << 20 {
                return from;
            }
            back = (back * 4).min(1 << 20);
        }
    }

    /// 解析视口要显示的行。只读取 `MAX_VIEW_BYTES` 以内的数据。
    ///
    /// `top_byte` 应传入**精确行首**（由 UI 的行索引换算），这样视口永远
    /// 从整行开始，不会出现「滚动后同一行重复出现」。
    pub fn render_lines(
        &mut self,
        top_byte: u64,
        max_lines: usize,
        max_bytes: usize,
    ) -> Vec<RenderLine> {
        let total = self.len();
        if max_lines == 0 {
            return Vec::new();
        }
        // 空文件也必须显示一个可点击、可输入的空行。
        if total == 0 {
            return vec![RenderLine {
                number: 0,
                byte_start: 0,
                byte_len: 0,
                text: String::new(),
                valid_utf8: true,
                truncated: false,
                number_estimated: false,
            }];
        }
        // 允许 top == total：文件以 \n 结尾时，最后一行是空行。
        let top = top_byte.min(total);
        let line_start = self.line_start_before(top);

        let want = max_bytes.min(MAX_VIEW_BYTES);
        let mut data = vec![0u8; want + 4];
        let n = self.read_at(line_start, &mut data);
        // 尾部对齐到 UTF-8 字符边界，避免把中文/emoji 切成替换字符。
        let keep = utf8::prev_boundary(&data, n.min(want));
        data.truncate(keep);
        let (ranges, incomplete) = utf8::split_lines(&data);

        let (base, estimated) = {
            let mut rd = LogicalReader {
                source: &mut self.source,
                pt: &self.pieces,
            };
            let res = self.lines.resolve(&mut rd, line_start);
            (res.line(), !res.is_exact())
        };
        // 视口落在文件末尾（末尾换行后的空行）时，ranges 为空但仍需一行。
        if ranges.is_empty() {
            return vec![RenderLine {
                number: base,
                byte_start: line_start,
                byte_len: 0,
                text: String::new(),
                valid_utf8: true,
                truncated: false,
                number_estimated: estimated,
            }];
        }
        let mut out = Vec::with_capacity(max_lines.min(ranges.len()));
        for (k, &(s, e)) in ranges.iter().take(max_lines).enumerate() {
            let mut bytes = &data[s..e];
            if line_start == 0 && k == 0 {
                bytes = utf8::strip_bom(bytes);
            }
            let (text, valid) = utf8::decode_line(bytes);
            out.push(RenderLine {
                number: base + k as u64,
                byte_start: line_start + s as u64,
                byte_len: (e - s) as u64,
                text,
                valid_utf8: valid,
                truncated: incomplete && k + 1 == ranges.len(),
                number_estimated: estimated,
            });
        }
        out
    }

    /// 查询某个逻辑偏移的行号（可能返回估算值）。
    pub fn line_number_at(&mut self, byte: u64) -> LineResolution {
        let mut rd = LogicalReader {
            source: &mut self.source,
            pt: &self.pieces,
        };
        self.lines.resolve(&mut rd, byte)
    }

    /// 后台精修完成后写入精确锚点。
    pub fn record_refinement(&mut self, byte: u64, line: u64) {
        self.lines.record_refinement(byte, line);
    }

    pub fn refine_start(&self, byte: u64) -> (u64, u64) {
        self.lines.anchor_before(byte)
    }

    pub fn estimate_line(&self, byte: u64) -> u64 {
        self.lines.estimate(byte)
    }

    /// 替换逻辑区间，自动登记撤销记录并使失效的行锚点作废。
    pub fn replace(&mut self, start: u64, end: u64, text: &[u8]) -> u64 {
        let rec = self.pieces.replace(start, end, text);
        let new_cursor = start + text.len() as u64;
        self.lines.invalidate_after(rec.start.min(new_cursor));
        self.undo.push(rec);
        self.redo.clear();
        self.dirty = true;
        new_cursor
    }

    /// 预读下一次撤销/重做会替换掉的区间长度，**不改动文档**。
    ///
    /// UI 层必须在变更前读 `[start, start+old_len)` 的换行数、变更后读
    /// `[start, start+new_len)` 的换行数，才能复用普通输入的
    /// `apply_index_edit()` 增量维护全文件行索引。没有这个快照，撤销时
    /// 只能把 `line_index` 整个丢弃并后台重建：总行数瞬间从精确值跌回
    /// 估算值、`ensure_cursor_visible()` 按估算把视口拉到错误位置、索引
    /// tick 每轮重绘 —— 三者叠加就是「快捷键撤销/重做会闪烁」。
    pub fn peek_undo_span(&self) -> Option<EditSpan> {
        self.undo.last().map(|r| edit_span(r, true))
    }

    pub fn peek_redo_span(&self) -> Option<EditSpan> {
        self.redo.last().map(|r| edit_span(r, false))
    }

    pub fn undo(&mut self) -> Option<u64> {
        let rec = self.undo.pop()?;
        let start = rec.start;
        self.pieces.apply_undo(&rec);
        self.lines.invalidate_after(start);
        self.redo.push(rec);
        self.dirty = true;
        Some(start)
    }

    pub fn redo(&mut self) -> Option<u64> {
        let rec = self.redo.pop()?;
        let start = rec.start;
        let end = match rec.inserted {
            Some(p) => start + p.len,
            None => start,
        };
        self.pieces.apply_redo(&rec);
        self.lines.invalidate_after(start);
        self.undo.push(rec);
        self.dirty = true;
        Some(end)
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn snapshot(&self) -> io::Result<Snapshot> {
        Ok(Snapshot {
            source: self.source.clone_handle()?,
            pieces: self.pieces.clone(),
        })
    }

    /// 原子保存：流式写临时文件后 `rename`。
    ///
    /// 内存恒定；已打开的原始 fd 仍指向旧 inode，因此 piece table、
    /// 撤销栈、后台搜索快照在保存后依然有效。
    /// 未命名缓冲区没有路径，返回 `InvalidInput`，由 UI 先询问文件名。
    pub fn save(&mut self) -> io::Result<u64> {
        if self.is_untitled() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "尚未指定文件名",
            ));
        }
        let target = self.path.clone();
        let n = self.write_atomic(&target)?;
        self.dirty = false;
        Ok(n)
    }

    /// 另存到指定路径（首次保存未命名缓冲区也走这里）：
    /// 写完才把 `path` 指过去，保存失败时原状态不变。
    pub fn save_to(&mut self, path: impl AsRef<Path>) -> io::Result<u64> {
        let target = path.as_ref().to_path_buf();
        if target.as_os_str().is_empty() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "文件名为空"));
        }
        let n = self.write_atomic(&target)?;
        self.path = target;
        self.dirty = false;
        Ok(n)
    }

    /// 流式写出到目标路径（临时文件 + rename），**不改动** `path` / `dirty`。
    fn write_atomic(&mut self, target: &Path) -> io::Result<u64> {
        let name = target
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "untitled".into());
        let tmp = target.with_file_name(format!(
            ".{}.owleditor-{}.tmp",
            name,
            std::process::id()
        ));
        let total = self.len();
        let result = (|| -> io::Result<()> {
            let mut out = File::create(&tmp)?;
            let mut buf = vec![0u8; 64 * 1024];
            let mut off = 0u64;
            while off < total {
                let n = self.read_at(off, &mut buf);
                if n == 0 {
                    break;
                }
                out.write_all(&buf[..n])?;
                off += n as u64;
            }
            out.flush()?;
            out.sync_all()?;
            drop(out);
            fs::rename(&tmp, target)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        result.map(|_| total)
    }

    pub fn stats(&self) -> Stats {
        let cached = self.source.cached_bytes();
        let add = self.pieces.add_bytes();
        let anchors = self.lines.len();
        Stats {
            logical_len: self.len(),
            original_len: self.source.len(),
            cached_bytes: cached,
            add_bytes: add,
            anchors,
            scanned_bytes: self.lines.scanned_bytes,
            resolve_calls: self.lines.resolve_calls,
            refinements: self.lines.refinements,
            undo_depth: self.undo.len(),
            redo_depth: self.redo.len(),
            dirty: self.dirty,
            // 128 KiB 覆盖各种临时缓冲（视口、查找窗口、行首回退等）。
            heap_upper_bound: cached + add + anchors * 16 + self.undo.len() * 64 + MAX_VIEW_BYTES + 128 * 1024,
        }
    }
}

impl ByteReader for Document {
    fn len(&self) -> u64 {
        self.pieces.logical_len()
    }
    fn read_at(&mut self, off: u64, buf: &mut [u8]) -> usize {
        Document::read_at(self, off, buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, data: &[u8]) {
        fs::write(path, data).unwrap();
    }

    #[test]
    fn open_large_sparse_file_is_instant_and_bounded() {
        let dir = std::env::temp_dir().join(format!("owleditor-core-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("10g.sparse");
        let f = File::create(&path).unwrap();
        f.set_len(10 * 1024 * 1024 * 1024).unwrap();
        drop(f);

        let mut doc = Document::open(&path).unwrap();
        assert_eq!(doc.len(), 10 * 1024 * 1024 * 1024);
        let s = doc.stats();
        assert_eq!(s.cached_bytes, 0, "打开时不应读入任何块");
        assert_eq!(s.scanned_bytes, 0, "打开时不应扫描任何换行");
        assert!(s.heap_upper_bound < 2 * 1024 * 1024);

        // 只看第一屏
        let lines = doc.render_lines(0, 40, 64 * 1024);
        let s2 = doc.stats();
        assert!(s2.scanned_bytes <= 64 * 1024, "只应解析视口");
        assert!(s2.cached_bytes <= crate::chunks::MAX_CACHED_BYTES);
        let _ = lines;

        // 直接跳到 9 GiB 处再看一屏，内存仍在同一上界内
        doc.render_lines(9 * 1024 * 1024 * 1024, 40, 64 * 1024);
        let s3 = doc.stats();
        assert!(s3.cached_bytes <= crate::chunks::MAX_CACHED_BYTES);
        assert!(s3.heap_upper_bound < 2 * 1024 * 1024);
        // 远距离跳转走估算，不会同步扫描整个前缀
        assert!(s3.scanned_bytes < 8 * 1024 * 1024, "不得全量扫描");

        fs::remove_file(&path).unwrap();
        let _ = fs::remove_dir(&dir);
    }

    #[test]
    fn edit_undo_redo_and_save_roundtrip() {
        let dir = std::env::temp_dir().join(format!("owleditor-core-rt-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.txt");
        write(&path, b"one\ntwo\nthree\n");

        let mut doc = Document::open(&path).unwrap();
        let cursor = doc.replace(4, 7, "TWO".as_bytes());
        assert_eq!(cursor, 7);
        assert_eq!(doc.read_range(0, 14), b"one\nTWO\nthree\n");
        assert!(doc.dirty);

        doc.undo();
        assert_eq!(doc.read_range(0, 14), b"one\ntwo\nthree\n");
        doc.redo();
        assert_eq!(doc.read_range(0, 14), b"one\nTWO\nthree\n");

        doc.save().unwrap();
        assert!(!doc.dirty);
        assert_eq!(fs::read(&path).unwrap(), b"one\nTWO\nthree\n");
        // 保存后撤销栈仍有效（原始 fd 仍指向旧 inode）
        doc.undo();
        assert_eq!(doc.read_range(0, 14), b"one\ntwo\nthree\n");

        fs::remove_file(&path).unwrap();
        let _ = fs::remove_dir(&dir);
    }

    #[test]
    fn utf8_cjk_lines_render_correctly() {
        let dir = std::env::temp_dir().join(format!("owleditor-core-u8-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("u.txt");
        write(&path, "第一行 abc\n第二行 😀\n第三行\n".as_bytes());
        let mut doc = Document::open(&path).unwrap();
        let lines = doc.render_lines(0, 10, 4096);
        // 末尾换行后还有一个可编辑空行（编辑器语义），共 4 行。
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[0].text, "第一行 abc");
        assert_eq!(lines[1].text, "第二行 😀");
        assert_eq!(lines[3].text, "");
        assert!(lines.iter().all(|l| l.valid_utf8));
        assert_eq!(lines[1].number, 1);
        fs::remove_file(&path).unwrap();
        let _ = fs::remove_dir(&dir);
    }

    #[test]
    fn snapshot_sees_edits_without_blocking_ui() {
        let dir = std::env::temp_dir().join(format!("owleditor-core-snap-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.txt");
        write(&path, b"alpha beta gamma\n");
        let mut doc = Document::open(&path).unwrap();
        doc.replace(6, 10, b"BETA");
        let mut snap = doc.snapshot().unwrap();
        let mut buf = [0u8; 64];
        let n = ByteReader::read_at(&mut snap, 0, &mut buf);
        assert_eq!(&buf[..n], b"alpha BETA gamma\n");
        fs::remove_file(&path).unwrap();
        let _ = fs::remove_dir(&dir);
    }

    #[test]
    fn untitled_buffer_edits_then_saves_to_chosen_path() {
        let dir = std::env::temp_dir().join(format!("owleditor-core-un-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let target = dir.join("named.txt");

        // 没有打开任何文件也能直接编辑。
        let mut doc = Document::untitled().unwrap();
        assert!(doc.is_untitled());
        assert!(doc.save().is_err(), "未命名缓冲区没有路径，不能直接保存");
        doc.replace(0, 0, b"hello\nworld\n");
        assert_eq!(doc.read_range(0, 12), b"hello\nworld\n");

        // 第一次保存：由 UI 选中路径后调用 save_to。
        let n = doc.save_to(&target).unwrap();
        assert_eq!(n, 12);
        assert!(!doc.is_untitled());
        assert!(!doc.dirty);
        assert_eq!(fs::read(&target).unwrap(), b"hello\nworld\n");

        // 之后 Ctrl+S 直接写回同一路径。
        doc.replace(12, 12, b"third\n");
        doc.save().unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"hello\nworld\nthird\n");

        fs::remove_file(&target).unwrap();
        let _ = fs::remove_dir(&dir);
    }

    #[test]
    fn undo_redo_span_matches_actual_content_change() {
        let dir = std::env::temp_dir().join(format!("owleditor-core-span-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.txt");
        fs::write(&path, b"alpha\nbeta\n").unwrap();
        let mut doc = Document::open(&path).unwrap();
        // 插入 "XX\n"：撤销前该区间长 3，撤销后长 0，正好少一个换行。
        doc.replace(6, 6, b"XX\n");
        let span = doc.peek_undo_span().unwrap();
        assert_eq!(span, EditSpan { start: 6, old_len: 3, new_len: 0 });
        assert_eq!(doc.read_range(span.start, span.old_len as usize), b"XX\n");
        assert_eq!(doc.undo(), Some(6));
        assert_eq!(doc.read_range(0, 11), b"alpha\nbeta\n");
        let rspan = doc.peek_redo_span().unwrap();
        assert_eq!(rspan, EditSpan { start: 6, old_len: 0, new_len: 3 });
        assert_eq!(doc.redo(), Some(9));
        assert_eq!(doc.read_range(0, 14), b"alpha\nXX\nbeta\n");
        fs::remove_file(&path).unwrap();
        let _ = fs::remove_dir(&dir);
    }

    #[test]
    fn deletion_span_restores_removed_length() {
        let dir = std::env::temp_dir().join(format!("owleditor-core-del-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.txt");
        fs::write(&path, b"one\ntwo\nthree\n").unwrap();
        let mut doc = Document::open(&path).unwrap();
        doc.replace(4, 8, b""); // 删掉 "two\n"
        let span = doc.peek_undo_span().unwrap();
        assert_eq!(span, EditSpan { start: 4, old_len: 0, new_len: 4 });
        doc.undo();
        assert_eq!(doc.read_range(0, 14), b"one\ntwo\nthree\n");
        fs::remove_file(&path).unwrap();
        let _ = fs::remove_dir(&dir);
    }
}
