//! 鼠标命中 → 文本字节偏移的纯逻辑（不依赖 Pango / 显示后端）。
//!
//! Pango 的 `Layout::xy_to_index(x, y)` 返回三项：`inside`、`index`、
//! `trailing`。`index` 是**命中字符的起始字节**，`trailing` 表示落点在该字符
//! 的右半边（以及整行末尾的空白处）。旧实现直接把 `index` 当光标位置、
//! 完全丢掉 `trailing`，于是：
//!
//! - 点在最后一个字符的右半边 → 光标停在该字符**之前**；
//! - 点在整行右侧空白 → 同样停在最后一个字符之前。
//!
//! 用户反馈「鼠标点击行末，永远在最后一个字符的前一个，永远不在最后」
//! 就是这两个分支。这里把换算固化成可测纯函数。

/// 前进到 `i` 之后的下一个 UTF-8 字符边界。
fn next_boundary(bytes: &[u8], mut i: usize) -> usize {
    if i >= bytes.len() {
        return bytes.len();
    }
    i += 1;
    while i < bytes.len() && (bytes[i] & 0b1100_0000) == 0b1000_0000 {
        i += 1;
    }
    i
}

/// 把 `xy_to_index` 的结果换算成光标应落在的文本内字节偏移。
///
/// - `index` / `trailing`：Pango 原样传入（index 可能为负，已做钳制）。
/// - `clicked_past_end`：点击 x 是否超过该行文本的实际右边界。落在行末
///   右侧空白时，光标应直接到行尾，而不是停在最后一个字符前。
/// - 返回值保证落在 `[0, text.len()]` 且位于 UTF-8 字符边界上。
pub fn hit_byte(text: &str, index: i32, trailing: i32, clicked_past_end: bool) -> usize {
    let bytes = text.as_bytes();
    let len = bytes.len();
    let mut i = (index.max(0) as usize).min(len);
    if clicked_past_end {
        return len;
    }
    if trailing != 0 {
        i = next_boundary(bytes, i);
    }
    // 防御：index 落在字符中间时对齐到边界（最多回退 3 字节）。
    while i > 0 && i < len && (bytes[i] & 0b1100_0000) == 0b1000_0000 {
        i -= 1;
    }
    i.min(len)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trailing_at_last_char_moves_past_it() {
        // "abc" 点在 'c' 的右半：index=2 trailing=1 -> 3（行尾）
        assert_eq!(hit_byte("abc", 2, 1, false), 3);
        // 点在 'c' 的左半：仍是 2
        assert_eq!(hit_byte("abc", 2, 0, false), 2);
        // 点在 'a' 的右半：-> 1
        assert_eq!(hit_byte("abc", 0, 1, false), 1);
    }

    #[test]
    fn clicking_blank_past_line_end_goes_to_end() {
        assert_eq!(hit_byte("abc", 2, 1, true), 3);
        assert_eq!(hit_byte("中文", 0, 0, true), 6);
        assert_eq!(hit_byte("", 0, 0, true), 0);
    }

    #[test]
    fn trailing_advances_a_whole_utf8_char() {
        // "中文" = [0..3]="中", [3..6]="文"
        assert_eq!(hit_byte("中文", 0, 1, false), 3);
        assert_eq!(hit_byte("中文", 3, 1, false), 6);
        // emoji 4 字节
        assert_eq!(hit_byte("a😀b", 1, 1, false), 5);
    }

    #[test]
    fn never_lands_inside_a_codepoint() {
        for (text, idx) in [("中文", 1usize), ("中文", 4), ("a😀b", 2), ("a😀b", 3)] {
            let got = hit_byte(text, idx as i32, 0, false);
            assert!(
                text.is_char_boundary(got),
                "{text:?} idx={idx} -> {got} 不在字符边界"
            );
        }
    }

    #[test]
    fn out_of_range_input_is_clamped() {
        assert_eq!(hit_byte("abc", -5, 0, false), 0);
        assert_eq!(hit_byte("abc", 999, 1, false), 3);
        assert_eq!(hit_byte("abc", 3, 1, false), 3);
    }
}
