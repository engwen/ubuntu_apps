//! 按需分块读取。
//!
//! 不使用 mmap：mmap 一个 10 GiB 文件虽然只占虚拟地址空间，但随即到来的
//! 缺页会把页缓存计入 RSS，且不同 compositor / 容器对 page cache 的回收
//! 行为不一致。这里改用 `pread` 式的 `FileExt::read_at`，物理内存上界由
//! 固定容量的 LRU 块缓存严格决定（128 × 64 KiB = 8 MiB）。

use crate::ByteReader;
use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::io;
use std::os::unix::fs::FileExt;
use std::path::Path;

/// 单个块的字节数。
pub const CHUNK_SIZE: u64 = 64 * 1024;
/// 缓存块数上限，决定内存上界。
pub const MAX_CACHED_CHUNKS: usize = 128;
/// 缓存的字节上界。
pub const MAX_CACHED_BYTES: usize = (CHUNK_SIZE as usize) * MAX_CACHED_CHUNKS;

pub struct ChunkSource {
    file: File,
    len: u64,
    cache: HashMap<u64, Vec<u8>>,
    order: VecDeque<u64>,
    /// 累计从磁盘读到的字节数（含被淘汰的块），用于验证"按需"。
    pub bytes_read: u64,
    /// 累计加载的块数。
    pub chunk_loads: u64,
}

impl ChunkSource {
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        let len = file.metadata()?.len();
        Ok(Self::from_file(file, len))
    }

    /// 零长度匿名文件：用于「未命名缓冲区」。
    ///
    /// 先创建临时文件再立刻 `unlink`，fd 依然有效，但磁盘上不再有可见路径，
    /// 进程退出时内核自动回收。这样未命名文档和真实文件走完全相同的读取
    /// 路径（`read_at` 返回 0），不需要为「无文件」新增一套分支。
    pub fn anonymous() -> io::Result<Self> {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let mut path = std::env::temp_dir();
        path.push(format!(".owleditor-anon-{}-{:x}", std::process::id(), nanos));
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)?;
        // 删除目录项但不关闭 fd：文件内容（此处为空）仍可读。
        let _ = std::fs::remove_file(&path);
        Ok(Self::from_file(file, 0))
    }

    fn from_file(file: File, len: u64) -> Self {
        Self {
            file,
            len,
            cache: HashMap::with_capacity(MAX_CACHED_CHUNKS + 1),
            order: VecDeque::with_capacity(MAX_CACHED_CHUNKS + 1),
            bytes_read: 0,
            chunk_loads: 0,
        }
    }

    /// 复制一个指向**同一 inode** 的独立读取句柄。
    ///
    /// 这是保存后仍可撤销/搜索的关键：保存用 `rename` 原子替换路径，
    /// 但已打开的 fd 仍指向保存前的文件块，内存中的 piece table 描述的
    /// 正是「保存前文件 + 编辑」。`read_at` 不移动文件偏移，因此多个
    /// 句柄可以安全地在不同线程并发读取。
    pub fn clone_handle(&self) -> io::Result<Self> {
        Ok(Self::from_file(self.file.try_clone()?, self.len))
    }

    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// 当前驻留内存的块字节数。任何时刻都 <= [`MAX_CACHED_BYTES`]。
    pub fn cached_bytes(&self) -> usize {
        self.cache.values().map(Vec::len).sum()
    }

    pub fn cached_chunks(&self) -> usize {
        self.cache.len()
    }

    fn touch(&mut self, idx: u64) {
        if let Some(pos) = self.order.iter().position(|&x| x == idx) {
            self.order.remove(pos);
        }
        self.order.push_back(idx);
    }

    fn load(&mut self, idx: u64) -> io::Result<()> {
        if self.cache.contains_key(&idx) {
            self.touch(idx);
            return Ok(());
        }
        let start = idx * CHUNK_SIZE;
        if start >= self.len {
            return Ok(());
        }
        let want = (self.len - start).min(CHUNK_SIZE) as usize;
        let mut buf = vec![0u8; want];
        // read_at 是位置读取，不修改共享的文件偏移量。
        let mut filled = 0usize;
        while filled < want {
            match self.file.read_at(&mut buf[filled..], start + filled as u64)? {
                0 => break,
                n => filled += n,
            }
        }
        buf.truncate(filled);
        self.bytes_read += filled as u64;
        self.chunk_loads += 1;

        if self.cache.len() >= MAX_CACHED_CHUNKS {
            if let Some(old) = self.order.pop_front() {
                self.cache.remove(&old);
            }
        }
        self.cache.insert(idx, buf);
        self.order.push_back(idx);
        Ok(())
    }

    /// 从任意偏移读取，自动跨块拼接。短读只发生在文件末尾。
    pub fn read_at(&mut self, off: u64, buf: &mut [u8]) -> io::Result<usize> {
        if off >= self.len || buf.is_empty() {
            return Ok(0);
        }
        let mut written = 0usize;
        while written < buf.len() {
            let pos = off + written as u64;
            if pos >= self.len {
                break;
            }
            let idx = pos / CHUNK_SIZE;
            let inner = (pos % CHUNK_SIZE) as usize;
            self.load(idx)?;
            let n = match self.cache.get(&idx) {
                Some(chunk) if inner < chunk.len() => {
                    let n = (chunk.len() - inner).min(buf.len() - written);
                    buf[written..written + n].copy_from_slice(&chunk[inner..inner + n]);
                    n
                }
                _ => break,
            };
            if n == 0 {
                break;
            }
            written += n;
        }
        Ok(written)
    }

    pub fn read_range(&mut self, off: u64, len: usize) -> io::Result<Vec<u8>> {
        let mut out = vec![0u8; len];
        let n = self.read_at(off, &mut out)?;
        out.truncate(n);
        Ok(out)
    }
}

impl ByteReader for ChunkSource {
    fn len(&self) -> u64 {
        self.len
    }

    fn read_at(&mut self, off: u64, buf: &mut [u8]) -> usize {
        ChunkSource::read_at(self, off, buf).unwrap_or(0)
    }
}
