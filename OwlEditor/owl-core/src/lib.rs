//! owl-core — OwlEditor 的文档内核。
//!
//! 设计约束（对应硬性指标）：
//!
//! 1. **内存不随文件体积上涨**：原始文件从不整体读入，只通过
//!    [`ChunkSource`] 按 64 KiB 块读取并缓存固定数量（8 MiB）的块；
//!    编辑内容通过 [`PieceTable`] 以增量形式保存。堆占用上界 =
//!    块缓存 + 已插入文本 + 有界行锚点 + 视口文本，与文件长度无关。
//! 2. **不一次性扫描全文件换行**：[`LazyLineIndex`] 只保存最多
//!    [`lines::MAX_ANCHORS`] 个稀疏锚点，且仅在需要显示/定位某个
//!    区域时从最近的锚点向前按需扫描一小段。打开文件只解析第一屏。
//! 3. **UTF-8 优先**：[`utf8`] 提供字节边界与行切分的纯函数。
//! 4. **流式查找与原子保存**：[`search`] 与 [`Document::save`] 都以
//!    固定缓冲顺序处理，不产生与文件大小成比例的分配。

pub mod chunks;
pub mod document;
pub mod hit;
pub mod index;
pub mod lines;
pub mod pieces;
pub mod preedit;
pub mod scroll;
pub mod search;
pub mod utf8;

pub use chunks::{ChunkSource, CHUNK_SIZE, MAX_CACHED_CHUNKS};
pub use document::{Document, EditSpan, RenderLine, Snapshot, Stats, MAX_VIEW_BYTES};
pub use index::{IndexBuilder, LineIndex, INDEX_BATCH, INDEX_STRIDE};
pub use lines::{LazyLineIndex, LineResolution, RefineJob, MAX_ANCHORS, MAX_SYNC_SCAN, REFINE_BATCH};
pub use pieces::{Piece, PieceSource, PieceTable, UndoRecord};
pub use preedit::{compose as compose_preedit, Composed};
pub use search::{Hit, SearchOutcome, SearchQuery};

/// 顺序字节读取抽象。行索引、查找、快照都基于它，保证核心逻辑
/// 与「文件 / 文档 / 快照」三种数据源解耦，便于独立测试。
pub trait ByteReader {
    /// 逻辑总长度（字节）。
    fn len(&self) -> u64;
    /// 从 `off` 起最多读满 `buf`，返回实际读到的字节数。
    fn read_at(&mut self, off: u64, buf: &mut [u8]) -> usize;
}

/// 一个文档位置的解析结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Position {
    /// 0 基行号。
    pub line: u64,
    /// 行内字节列（UTF-8 字节，不是字符数）。
    pub col: u64,
}
