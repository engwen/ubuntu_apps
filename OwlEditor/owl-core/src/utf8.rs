//! UTF-8 边界与行切分。
//!
//! 换行符 `\n`（以及 `\r\n` 的 `\r`）都是 ASCII，因此块边界永远不会切断
//! "行"这个概念；但会切断一个多字节字符。视口解码前必须把区间两端对齐到
//! 字符边界，否则中文 / emoji 会显示成替换字符。

/// 该字节是否可能是一个 UTF-8 字符的起始字节。
#[inline]
pub fn is_char_boundary(b: u8) -> bool {
    b & 0b1100_0000 != 0b1000_0000
}

/// 把偏移 `i` 向后退到 <= i 的最近字符边界。
pub fn prev_boundary(bytes: &[u8], i: usize) -> usize {
    let mut i = i.min(bytes.len());
    while i > 0 && i < bytes.len() && !is_char_boundary(bytes[i]) {
        i -= 1;
    }
    i
}

/// 把偏移 `i` 向前进到 >= i 的最近字符边界。
pub fn next_boundary(bytes: &[u8], i: usize) -> usize {
    let mut i = i.min(bytes.len());
    while i < bytes.len() && !is_char_boundary(bytes[i]) {
        i += 1;
    }
    i
}

/// 解码一行文本。UTF-8 优先：合法序列原样保留，非法字节用 U+FFFD 替换，
/// 且在状态栏给出提示。不做 GBK 猜测——Notepad-- 的乱码误判就来自编码嗅探。
pub fn decode_line(bytes: &[u8]) -> (String, bool) {
    match std::str::from_utf8(bytes) {
        Ok(s) => (s.to_string(), true),
        Err(_) => (String::from_utf8_lossy(bytes).into_owned(), false),
    }
}

/// 去掉 UTF-8 BOM，返回剩余内容。
pub fn strip_bom(bytes: &[u8]) -> &[u8] {
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        &bytes[3..]
    } else {
        bytes
    }
}

/// 行内字节列 -> 显示字符列（按 Unicode 标量值计数）。
pub fn byte_col_to_char_col(bytes: &[u8], byte_col: usize) -> usize {
    let byte_col = byte_col.min(bytes.len());
    bytes[..byte_col]
        .iter()
        .filter(|&&b| is_char_boundary(b))
        .count()
}

/// 显示字符列 -> 行内字节列。
pub fn char_col_to_byte_col(bytes: &[u8], char_col: usize) -> usize {
    let mut cols = 0usize;
    for (i, &b) in bytes.iter().enumerate() {
        // 只在真正的字符起始字节处计数，不能落在多字节字符的续字节上。
        if !is_char_boundary(b) {
            continue;
        }
        if cols == char_col {
            return i;
        }
        cols += 1;
    }
    bytes.len()
}

/// 下一个字符的字节长度（非法首字节按 1 处理）。
#[inline]
pub fn char_len(b: u8) -> usize {
    match b {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF7 => 4,
        _ => 1,
    }
}

/// 把字节区间按 `\n` 切成若干行，返回 `(start, end)`，行内容不含 `\n`，
/// 且 CRLF 的 `\r` 会被剥掉。`incomplete` 表示末行是否因区间截断而未遇到换行。
///
/// 采用**编辑器语义**：文件以 `\n` 结尾时，末尾仍保留一个可编辑空行
/// （Kate / EmEditor / Notepad-- 都是这样），否则光标无法停在最后一行。
pub fn split_lines(data: &[u8]) -> (Vec<(usize, usize)>, bool) {
    let mut out = Vec::new();
    let mut start = 0usize;
    for (i, &b) in data.iter().enumerate() {
        if b == b'\n' {
            let mut end = i;
            if end > start && data[end - 1] == b'\r' {
                end -= 1;
            }
            out.push((start, end));
            start = i + 1;
        }
    }
    let incomplete = start < data.len();
    if incomplete {
        let mut end = data.len();
        if end > start && data[end - 1] == b'\r' {
            end -= 1;
        }
        out.push((start, end));
    } else if data.last() == Some(&b'\n') {
        // 末尾换行之后还有一个空行；其 byte_start 正好是下一行的行首。
        out.push((data.len(), data.len()));
    }
    (out, incomplete)
}

/// 在一块数据里找最后一个 `\n` 的位置，用于从任意字节偏移回退到行首。
pub fn last_newline(data: &[u8]) -> Option<usize> {
    data.iter().rposition(|&b| b == b'\n')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boundaries_around_cjk() {
        let s = "ab中文😀cd".as_bytes();
        assert_eq!(s.len(), 2 + 6 + 4 + 2);
        // "中" 起始在 2，占 2..5
        assert!(!is_char_boundary(s[3]));
        assert_eq!(prev_boundary(s, 4), 2);
        assert_eq!(next_boundary(s, 4), 5);
        assert_eq!(byte_col_to_char_col(s, s.len()), 7); // a b 中 文 😀 c d
        assert_eq!(char_col_to_byte_col(s, 3), 5); // 第 3 个字符「文」的起始字节
    }

    #[test]
    fn byte_char_col_roundtrip() {
        let s = "a中b😀c".as_bytes();
        assert_eq!(byte_col_to_char_col(s, s.len()), 5);
        for col in 0..=5 {
            let b = char_col_to_byte_col(s, col);
            assert_eq!(byte_col_to_char_col(s, b), col, "col={col}");
        }
    }

    #[test]
    fn split_lines_handles_crlf_and_trailing() {
        let d = b"one\r\ntwo\nthree";
        let (lines, incomplete) = split_lines(d);
        assert_eq!(lines, vec![(0, 3), (5, 8), (9, 14)]);
        assert!(incomplete);
        // 编辑器语义：末尾换行之后还有一个可编辑空行
        let (lines2, inc2) = split_lines(b"a\nb\n");
        assert_eq!(lines2, vec![(0, 1), (2, 3), (4, 4)]);
        assert!(!inc2);
    }

    #[test]
    fn invalid_utf8_is_flagged() {
        let (text, valid) = decode_line(b"ab\xFF\xFEcd");
        assert!(!valid);
        assert!(text.contains('\u{FFFD}'));
        let (text2, valid2) = decode_line("中文".as_bytes());
        assert!(valid2);
        assert_eq!(text2, "中文");
    }
}
