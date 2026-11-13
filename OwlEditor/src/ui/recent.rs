//! 最近打开文件的历史记录（与 UI 解耦的纯逻辑，便于单元测试）。
//!
//! 设计要点：
//!   * 最多保留 [`MAX_RECENT`] 条，最新打开的排在最前。
//!   * 用绝对路径去重；同一个文件不论以什么路径形式打开都只保留一条。
//!   * 持久化文件每行一个绝对路径，加载时忽略空行与不存在的文件仍可正常读。
//!   * 丢失检测只负责「数据层面」：标记不存在的条目并移除，是否弹窗提醒由
//!     调用方决定（UI 层），这样纯逻辑在 headless 环境下也能测试。

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

/// 最近打开文件的最大条数（用户要求 20）。
pub const MAX_RECENT: usize = 20;

/// 最近打开文件列表，最近在前。
#[derive(Debug, Clone, Default)]
pub struct RecentFiles {
    entries: Vec<PathBuf>,
}

impl RecentFiles {
    /// 空列表。
    pub fn new() -> Self {
        Self { entries: Vec::new() }
    }

    /// 从任意按行读取的来源构建（每行一个路径）。空行被忽略。
    pub fn from_reader<R: BufRead>(reader: R) -> Self {
        let mut entries = Vec::new();
        for line in reader.lines().map_while(Result::ok) {
            let t = line.trim();
            if !t.is_empty() {
                entries.push(PathBuf::from(t));
            }
        }
        entries.truncate(MAX_RECENT);
        Self { entries }
    }

    /// 从磁盘文件加载；文件不存在 / 读取失败时退化为空列表。
    pub fn from_file(path: &Path) -> Self {
        match std::fs::File::open(path) {
            Ok(f) => Self::from_reader(std::io::BufReader::new(f)),
            Err(_) => Self::new(),
        }
    }

    /// 打开一个文件：去重并放到最前，超出上限截断。
    pub fn push(&mut self, p: &Path) {
        let key = normalize(p);
        self.entries.retain(|e| normalize(e) != key);
        self.entries.insert(0, p.to_path_buf());
        if self.entries.len() > MAX_RECENT {
            self.entries.truncate(MAX_RECENT);
        }
    }

    /// 从列表里移除指定文件（按规范化路径匹配）。
    pub fn remove(&mut self, p: &Path) {
        let key = normalize(p);
        self.entries.retain(|e| normalize(e) != key);
    }

    /// 移除所有已不存在 / 不是普通文件的条目，返回被移除的（按原顺序）。
    pub fn prune_missing(&mut self) -> Vec<PathBuf> {
        let mut removed = Vec::new();
        self.entries.retain(|e| {
            let exists = e.is_file();
            if !exists {
                removed.push(e.clone());
            }
            exists
        });
        removed
    }

    /// 当前条目（只读视图，最近在前）。
    pub fn entries(&self) -> &[PathBuf] {
        &self.entries
    }

    /// 持久化到磁盘（必要时建目录）。
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut f = std::fs::File::create(path)?;
        for e in &self.entries {
            writeln!(f, "{}", e.display())?;
        }
        Ok(())
    }
}

/// 把路径规范化成可比较的键：能解析成真实路径就解析（处理符号链接 /
/// 相对段），否则退回为「绝对路径」；都失败就原样返回。
fn normalize(p: &Path) -> PathBuf {
    if let Ok(canonical) = p.canonicalize() {
        return canonical;
    }
    if p.is_absolute() {
        return p.to_path_buf();
    }
    std::env::current_dir()
        .map(|c| c.join(p))
        .unwrap_or_else(|_| p.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn push_most_recent_first_and_dedup() {
        let mut r = RecentFiles::new();
        r.push(Path::new("/a/old.txt"));
        r.push(Path::new("/b/mid.txt"));
        r.push(Path::new("/c/new.txt"));
        let e = r.entries();
        assert_eq!(e[0], PathBuf::from("/c/new.txt"));
        assert_eq!(e.len(), 3);

        // 重复打开最旧的，应被提到最前并去重
        r.push(Path::new("/a/old.txt"));
        let e = r.entries();
        assert_eq!(e.len(), 3);
        assert_eq!(e[0], PathBuf::from("/a/old.txt"));
    }

    #[test]
    fn caps_at_max() {
        let mut r = RecentFiles::new();
        for i in 0..(MAX_RECENT + 10) {
            r.push(&PathBuf::from(format!("/x/file{i}.txt")));
        }
        assert_eq!(r.entries().len(), MAX_RECENT);
        // 最前面是最后 push 的
        assert_eq!(r.entries()[0], PathBuf::from("/x/file29.txt"));
    }

    #[test]
    fn from_reader_ignores_blank_lines_and_caps() {
        // 每个 i 都会写入一行 /y/f{i}.log；i%3==0 时额外插入一个空行。
        // 因此真实条目共 25 行，空行被忽略，25>20 触发上限截断到 20。
        let mut body = String::new();
        for i in 0..(MAX_RECENT + 5) {
            if i % 3 == 0 {
                body.push('\n'); // 空行，应被忽略
            }
            body.push_str(&format!("/y/f{i}.log\n"));
        }
        let r = RecentFiles::from_reader(Cursor::new(body));
        assert_eq!(r.entries().len(), MAX_RECENT);
        // 文件顺序保持：第一个条目是 f0，截断后最后一个是 f19。
        assert_eq!(r.entries()[0], PathBuf::from("/y/f0.log"));
        assert_eq!(r.entries().last().unwrap(), &PathBuf::from("/y/f19.log"));
    }

    #[test]
    fn prune_missing_removes_and_reports() {
        let tmp = std::env::temp_dir().join(format!("owleditor_recent_test_{}.txt", std::process::id()));
        std::fs::write(&tmp, b"hello").unwrap();
        let missing = std::env::temp_dir().join(format!("owleditor_recent_missing_{}.txt", std::process::id()));
        // 确保它不存在
        let _ = std::fs::remove_file(&missing);

        let mut r = RecentFiles::new();
        r.push(&tmp);
        r.push(&missing);

        let removed = r.prune_missing();
        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0], missing);
        assert_eq!(r.entries().len(), 1);
        assert_eq!(r.entries()[0], tmp);

        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn remove_matches() {
        let mut r = RecentFiles::new();
        r.push(Path::new("/z/a.txt"));
        r.push(Path::new("/z/b.txt"));
        r.remove(Path::new("/z/a.txt"));
        assert_eq!(r.entries(), &[PathBuf::from("/z/b.txt")]);
    }

    #[test]
    fn save_roundtrip() {
        let dir = std::env::temp_dir().join(format!("owleditor_recent_dir_{}", std::process::id()));
        let file = dir.join("recent.txt");
        let _ = std::fs::remove_dir_all(&dir);

        let mut r = RecentFiles::new();
        r.push(Path::new("/p/one.rs"));
        r.push(Path::new("/p/two.rs"));
        r.save(&file).unwrap();

        let loaded = RecentFiles::from_file(&file);
        // push 把最新插到最前：先 one 后 two，故存盘与读出均为 [two, one]。
        assert_eq!(loaded.entries(), &[PathBuf::from("/p/two.rs"), PathBuf::from("/p/one.rs")]);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
