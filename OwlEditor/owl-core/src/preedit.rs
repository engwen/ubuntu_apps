//! 输入法预编辑串在显示行内的插入与字节偏移映射（纯逻辑，不依赖 GTK）。
//!
//! ## 为什么要「让位」
//!
//! 早期实现把预编辑串用一个独立 Pango layout 直接画在光标坐标上，正文并不
//! 移动，于是拼音串压在本行后续文字上（用户截图里蓝色「326度」叠在
//! 「-1-05」上），每敲一个字后面的文字还会跟着抖。正确做法是把预编辑串
//! **拼进该行的显示文本**，由 Pango 自然把后续字符右移，文字统一用正文色。
//!
//! 拼接后布局的字节偏移与文档真实字节偏移不再一致，因此同时提供双向映射：
//! 选区 / 查找高亮 / 鼠标命中测试都必须经过它，否则预编辑期间点一下光标
//! 就会跳过整个拼音串。
//!
//! 该模块只做字符串与整数运算，可在无显示后端的环境里做确定性回归验证。

/// 预编辑串插入后的显示文本与映射信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Composed {
    /// 合成后的显示文本（= 前半 + 预编辑串 + 后半）。
    pub text: String,
    /// 插入点（原行内字节偏移，已对齐 UTF-8 边界）。
    pub off: usize,
    /// 预编辑串字节长度。
    pub shift: usize,
    /// 光标在预编辑串内的字节偏移。
    pub caret: usize,
}

impl Composed {
    /// 文档/选区字节偏移 → 合成布局字节偏移。
    pub fn to_layout_byte(&self, b: usize) -> usize {
        if b <= self.off {
            b
        } else {
            (b + self.shift).min(self.text.len())
        }
    }

    /// 合成布局字节偏移 → 文档字节偏移。
    /// 落在预编辑串内部时回到插入点，绝不让光标进入未提交文本。
    pub fn from_layout_byte(&self, b: usize) -> usize {
        if b <= self.off {
            b
        } else if b <= self.off + self.shift {
            self.off
        } else {
            b.saturating_sub(self.shift)
        }
    }

    /// 光标在合成布局中的字节位置。
    pub fn caret_byte(&self) -> usize {
        (self.off + self.caret).min(self.text.len())
    }

    /// 预编辑串区间（合成布局字节偏移），用于画低对比度下划线。
    pub fn preedit_span(&self) -> (usize, usize) {
        (self.off, (self.off + self.shift).min(self.text.len()))
    }
}

/// 把预编辑串插入显示行的 `cursor_off` 处。
///
/// - `cursor_off` 是光标在本行内的**字节**偏移；
/// - `preedit_cursor` 来自 `IMContext::preedit_string()`，是**字符**位置，
///   这里按字符数换算成字节偏移，中文拼音串也不会错位。
/// - 插入点会向下对齐到 UTF-8 字符边界，绝不会把汉字切成两半。
pub fn compose(row_text: &str, cursor_off: usize, preedit: &str, preedit_cursor: i32) -> Composed {
    let mut off = cursor_off.min(row_text.len());
    while off > 0 && !row_text.is_char_boundary(off) {
        off -= 1;
    }
    let caret = preedit
        .char_indices()
        .nth(preedit_cursor.max(0) as usize)
        .map(|(i, _)| i)
        .unwrap_or(preedit.len());
    let mut text = String::with_capacity(row_text.len() + preedit.len());
    text.push_str(&row_text[..off]);
    text.push_str(preedit);
    text.push_str(&row_text[off..]);
    Composed { text, off, shift: preedit.len(), caret }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inserts_and_shifts_following_text() {
        // 偏移 4 = "2026" 之后、连字符之前
        let c = compose("2026-11-05 日志", 4, "duōshǎo", 3);
        assert_eq!(c.text, "2026duōshǎo-11-05 日志");
        assert_eq!(c.off, 4);
        assert_eq!(&c.text[..c.off], "2026");
        // 后续字符确实右移了，而不是被盖住
        assert_eq!(&c.text[c.off + c.shift..], "-11-05 日志");
    }

    #[test]
    fn byte_mapping_round_trips() {
        let row = "abc中文xyz";
        let c = compose(row, 3, "pinyin", 2);
        for b in 0..=row.len() {
            if !row.is_char_boundary(b) {
                continue;
            }
            let mapped = c.to_layout_byte(b);
            assert_eq!(c.from_layout_byte(mapped), b, "字节 {b} 往返不一致");
        }
        // 落在预编辑串内部的点击一律回到插入点
        for b in (c.off + 1)..(c.off + c.shift) {
            assert_eq!(c.from_layout_byte(b), c.off);
        }
    }

    #[test]
    fn utf8_boundary_is_never_split() {
        let row = "中文混排abc";
        // 故意给一个落在「文」中间的偏移
        let c = compose(row, 4, "ni", 1);
        assert!(c.text.is_char_boundary(c.off));
        assert!(c.text.is_char_boundary(c.off + c.shift));
        assert_eq!(c.text.chars().count(), row.chars().count() + "ni".chars().count());
    }

    #[test]
    fn caret_counts_characters_not_bytes() {
        let c = compose("start", 5, "中文拼", 2);
        // preedit_cursor=2 指第 2 个字符之后 -> 字节 6
        assert_eq!(c.caret, 6);
        assert_eq!(c.caret_byte(), 5 + 6);
        assert_eq!(c.preedit_span(), (5, 5 + 9));
    }

    #[test]
    fn empty_preedit_is_identity() {
        let c = compose("hello 世界", 6, "", 0);
        assert_eq!(c.text, "hello 世界");
        assert_eq!(c.shift, 0);
        assert_eq!(c.to_layout_byte(9), 9);
        assert_eq!(c.from_layout_byte(9), 9);
    }

    #[test]
    fn cursor_past_end_is_clamped() {
        let c = compose("短", 999, "abc", 3);
        assert_eq!(c.off, "短".len());
        assert_eq!(c.text, "短abc");
        assert_eq!(c.caret_byte(), "短abc".len());
    }
}
