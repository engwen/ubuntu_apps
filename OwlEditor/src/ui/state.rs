//! 界面共享状态。
//!
//! 与早期版本最大的区别：**视口由行号 `top_line` 驱动，而不是字节偏移**。
//! 之前按「估算平均行长 × 滚动值」换算字节偏移，`top_byte` 常落在行中间，
//! 回退到行首后同一行会被反复显示（用户看到的「重复行」），滚动条点击
//! 也会因为换算不闭合而跳回原处。现在借助后台全文件行索引做精确的
//! 行号 ↔ 字节互换，滚动、行号、跳转、状态栏全部使用同一套精确值。

use crate::ui::theme::Theme;
use gtk::prelude::*;
use gtk::pango;
use owl_core::{utf8, Document, Hit, LineIndex, RenderLine, MAX_VIEW_BYTES};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

pub const APP_ID: &str = "io.github.owl.owleditor";
pub const APP_NAME: &str = "OwlEditor";
/// 没有行索引时的临时估算行长，仅用于索引构建完成前。
const FALLBACK_AVG_LINE: u64 = 80;

pub type Shared = Rc<RefCell<AppState>>;

/// 一个显示行。关闭自动换行时与逻辑行一一对应；开启时长行会被软折行。
#[derive(Debug, Clone)]
pub struct DisplayRow {
    /// 对应的逻辑行下标（索引 `AppState::view`）。
    pub logical: usize,
    /// 该逻辑行内的第几个显示行（0 基）。
    pub sub: usize,
    /// 本显示行的文本。
    pub text: String,
    /// 本显示行第一个字节的逻辑偏移。
    pub byte_start: u64,
    /// 本显示行的字节长度。
    pub byte_len: u64,
    /// 逻辑行号（0 基）。
    pub number: u64,
    pub number_estimated: bool,
    pub truncated: bool,
}

pub struct AppState {
    pub doc: Option<Document>,
    pub path: Option<std::path::PathBuf>,
    /// 视口顶部的**逻辑行号**（自动换行时，是第一个可见显示行所属的逻辑行）。
    pub top_line: u64,
    /// 自动换行时，视口第一个显示行在 `top_line` 逻辑行内的子行下标。
    /// 没有它，按逻辑行滚动会一次跳过整条逻辑行的所有折行，
    /// 中间那些显示行永远显示不出来。
    pub top_sub: usize,
    /// `top_line` 这条逻辑行被折成了几个显示行（用于钳制 `top_sub`）。
    pub first_line_rows: usize,
    /// 打开以来见过的最大折行比（显示行数 / 逻辑行数），只增不减，
    /// 避免滚到短行区时滚动条长度来回变化、把滑块拽回去。
    pub wrap_ratio: f64,
    /// 光标逻辑字节偏移。
    pub cursor: u64,
    pub sel_anchor: Option<u64>,
    /// 水平滚动像素偏移。
    pub x_offset: f64,
    /// 上下移动时希望保持的显示列。
    pub goal_col: usize,

    pub width: i32,
    pub height: i32,
    pub line_height: i32,
    /// Latin 字符的**实测像素宽**（Pango layout 量出来，不是 approximate_char_width）。
    pub char_width: f64,
    /// 全角字符的实测像素宽；中文混排时光标/滚动必须用它，否则会逐字漂移。
    pub cjk_width: f64,
    pub gutter_width: i32,
    /// 上一次计算字体度量时的字体描述，避免每次 size_allocate 都重算造成抖动。
    pub metrics_key: String,
    pub font: pango::FontDescription,
    pub theme: Theme,

    pub view: Vec<RenderLine>,
    pub rows: Vec<DisplayRow>,
    pub view_dirty: bool,
    /// 视口内最长行的像素宽度，用于水平滚动范围。
    pub max_line_px: f64,
    /// 打开以来见过的全局最长行。水平范围只增不减，避免滚到短行区域时
    /// x_offset 被猛地 clamp 回 0（用户看到的「横向滚不动 / 突然弹回」）。
    pub doc_max_line_px: f64,

    /// 后台建立的全文件行索引。
    pub line_index: Option<LineIndex>,
    pub index_progress: f64,
    pub index_building: bool,
    /// 每次编辑 +1，防止后台把基于旧内容的索引装回来。
    pub revision: u64,

    pub show_line_numbers: bool,
    pub word_wrap: bool,
    pub read_only: bool,
    pub font_size: i32,
    pub line_ending: &'static str,
    pub encoding: String,

    pub preedit: String,
    pub preedit_cursor: i32,
    pub hits: Vec<Hit>,
    pub hit_index: usize,
    pub search_query: String,
    pub replace_text: String,
    pub case_sensitive: bool,
    pub search_msg: String,
    pub message: String,

    pub im: Option<gtk::IMMulticontext>,
    pub vadj: Option<gtk::Adjustment>,
    pub hadj: Option<gtk::Adjustment>,
    /// 横向滚动条控件句柄：自动换行打开时隐藏（无横向溢出），关闭时显示。
    pub hscroll: Option<gtk::Scrollbar>,
    pub search_cancel: Option<Arc<AtomicBool>>,
    pub index_cancel: Option<Arc<AtomicBool>>,

    /// 内容/光标变化后的统一刷新钩子（重绘 + 状态栏 + 滚动条 + 输入法光标）。
    pub on_change: Option<Rc<dyn Fn()>>,
    /// 请求后台建立/重建行索引。
    pub on_need_index: Option<Rc<dyn Fn()>>,
    /// 菜单动作分发（由窗口层实现）。
    pub on_action: Option<Rc<dyn Fn(&str)>>,
}

/// 所有状态修改都走这里：改完自动触发刷新，且不会造成 RefCell 重入。
pub fn with_state<F: FnOnce(&mut AppState)>(state: &Shared, f: F) {
    let hook = {
        let mut s = state.borrow_mut();
        f(&mut s);
        s.on_change.clone()
    };
    if let Some(h) = hook {
        h();
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

impl AppState {
    pub fn new() -> Self {
        let mut font = pango::FontDescription::from_string("Monospace 12");
        font.set_size(12 * pango::SCALE);
        Self {
            doc: None,
            path: None,
            top_line: 0,
            top_sub: 0,
            first_line_rows: 1,
            wrap_ratio: 1.0,
            cursor: 0,
            sel_anchor: None,
            x_offset: 0.0,
            goal_col: 0,
            width: 900,
            height: 600,
            line_height: 20,
            char_width: 9.0,
            cjk_width: 18.0,
            gutter_width: 72,
            metrics_key: String::new(),
            font,
            theme: Theme::dark(),
            view: Vec::new(),
            rows: Vec::new(),
            view_dirty: true,
            max_line_px: 0.0,
            doc_max_line_px: 0.0,
            line_index: None,
            index_progress: 0.0,
            index_building: false,
            revision: 0,
            show_line_numbers: true,
            // 默认自动换行（用户要求：打开长日志行时一屏看全，不用横向拖）。
            // 菜单「视图 → 自动换行」可随时关闭。
            word_wrap: true,
            read_only: false,
            font_size: 12,
            line_ending: "LF",
            encoding: "UTF-8".to_string(),
            preedit: String::new(),
            preedit_cursor: 0,
            hits: Vec::new(),
            hit_index: 0,
            search_query: String::new(),
            replace_text: String::new(),
            case_sensitive: false,
            search_msg: String::new(),
            message: String::new(),
            im: None,
            vadj: None,
            hadj: None,
            hscroll: None,
            search_cancel: None,
            index_cancel: None,
            on_change: None,
            on_need_index: None,
            on_action: None,
        }
    }

    pub fn len(&self) -> u64 {
        self.doc.as_ref().map(Document::len).unwrap_or(0)
    }

    pub fn has_doc(&self) -> bool {
        self.doc.is_some()
    }

    pub fn visible_lines(&self) -> usize {
        ((self.height / self.line_height.max(1)) as usize)
            .saturating_add(2)
            .max(4)
    }

    /// 可供正文使用的像素宽度。
    pub fn text_width(&self) -> f64 {
        (self.width as f64 - self.content_x()).max(80.0)
    }

    /// 正文起始 x（行号槽 + 左内边距）。
    pub fn content_x(&self) -> f64 {
        (if self.show_line_numbers { self.gutter_width as f64 } else { 0.0 }) + 8.0
    }

    // ------------------------------------------------------------ 行索引

    pub fn total_lines(&self) -> u64 {
        self.line_index
            .as_ref()
            .map(LineIndex::total_lines)
            .unwrap_or_else(|| (self.len() / FALLBACK_AVG_LINE).max(1) + 1)
    }

    pub fn index_exact(&self) -> bool {
        self.line_index.as_ref().map(|i| i.is_complete()).unwrap_or(false)
    }

    /// 精确求第 `line` 行的行首字节偏移（索引未建完时给出估算）。
    pub fn byte_of_line(&mut self, line: u64) -> u64 {
        let len = self.len();
        match (self.line_index.as_ref(), self.doc.as_mut()) {
            (Some(idx), Some(doc)) => idx.byte_of_line(doc, line).min(len),
            _ => (line.saturating_mul(FALLBACK_AVG_LINE)).min(len),
        }
    }

    pub fn line_of_byte(&mut self, byte: u64) -> u64 {
        match (self.line_index.as_ref(), self.doc.as_mut()) {
            (Some(idx), Some(doc)) => idx.line_of_byte(doc, byte),
            _ => byte / FALLBACK_AVG_LINE,
        }
    }

    /// 编辑后增量维护索引；无法精确维护时丢弃并请求后台重建。
    fn apply_index_edit(
        &mut self,
        start: u64,
        old_end: u64,
        new_end: u64,
        old_newlines: Option<u64>,
        new_newlines: u64,
    ) {
        self.revision += 1;
        let can = self.line_index.as_ref().map(|i| i.is_complete()).unwrap_or(false)
            && old_newlines.is_some();
        if can {
            if let Some(idx) = self.line_index.as_mut() {
                idx.apply_edit(start, old_end, new_end, old_newlines.unwrap(), new_newlines);
                return;
            }
        }
        self.line_index = None;
        self.index_progress = 0.0;
    }

    /// 读取区间内的换行数；区间过大时返回 None（放弃增量、后台重建）。
    fn newlines_in(&mut self, start: u64, end: u64) -> Option<u64> {
        let span = end.saturating_sub(start);
        if span > 4 * 1024 * 1024 {
            return None;
        }
        let data = self.doc.as_mut()?.read_range(start, span as usize);
        Some(data.iter().filter(|&&b| b == b'\n').count() as u64)
    }

    // ------------------------------------------------------------ 度量/视图

    /// 字体度量只在字体描述变化时重算。
    ///
    /// 之前每次 `size_allocate` 都用 `approximate_char_width()` 重算：这个值在中文字体
    /// 首次回退后会变，连带 `gutter_width` / `content_x()` 变，于是每敲一个字整行
    /// 文字就左右抖一下。现在改用 Pango layout **实测** Latin 与全角字符的像素宽，
    /// 并缓存到字体描述不变为止，度量完全稳定。
    pub fn update_metrics(&mut self, widget: &impl IsA<gtk::Widget>) {
        let key = self.font.to_string();
        if self.metrics_key == key && self.line_height > 0 {
            return;
        }
        self.metrics_key = key;

        let font = self.font.clone();
        let measure = |sample: &str| -> f64 {
            let layout = widget.create_pango_layout(Some(sample));
            layout.set_font_description(Some(&font));
            layout.index_to_pos(sample.len() as i32).x() as f64 / pango::SCALE as f64
        };
        let cw = measure("M").max(1.0);
        let cjk = measure("汉").max(cw);
        let ctx = widget.pango_context();
        let metrics = ctx.metrics(Some(&font), None);
        let lh = (metrics.height() / pango::SCALE).max(10) + 3;
        let gutter = (cw * 10.0) as i32 + 16;

        let changed = (self.char_width - cw).abs() > 0.01
            || (self.cjk_width - cjk).abs() > 0.01
            || self.line_height != lh
            || self.gutter_width != gutter;
        self.char_width = cw;
        self.cjk_width = cjk;
        self.line_height = lh;
        self.gutter_width = gutter;
        if changed {
            self.view_dirty = true;
        }
    }

    /// 文本的实际显示像素宽（ASCII 用实测 Latin 宽，全角用实测 CJK 宽）。
    /// 光标、水平滚动、最长行统计必须用它，不能再用 `display_cols * char_width`
    /// 的估算值，否则中英混排时会逐字累积误差、看起来像「跳动」。
    pub fn display_px(&self, s: &str) -> f64 {
        s.chars()
            .map(|c| {
                if c.is_control() {
                    0.0
                } else if c.len_utf8() > 1 {
                    self.cjk_width
                } else {
                    self.char_width
                }
            })
            .sum()
    }

    /// 重解析视口。只在视口变化时调用。
    pub fn refresh_view(&mut self) {
        if !self.view_dirty {
            return;
        }
        // 多取两条逻辑行：自动换行下 `scroll_by_display_rows` 需要靠已展开的
        // 显示行列表定位目标行，多几行可以避免一次滚动就落到估算分支。
        let max_lines = self.visible_lines() + 2;
        let top = self.byte_of_line(self.top_line);
        let exact = self.index_exact();

        let mut lines = match self.doc.as_mut() {
            Some(doc) => doc.render_lines(top, max_lines, MAX_VIEW_BYTES),
            None => vec![RenderLine {
                number: 0,
                byte_start: 0,
                byte_len: 0,
                text: String::new(),
                valid_utf8: true,
                truncated: false,
                number_estimated: false,
            }],
        };

        if !lines.is_empty() {
            let base = if exact {
                self.line_of_byte(lines[0].byte_start)
            } else {
                lines[0].number
            };
            for (k, l) in lines.iter_mut().enumerate() {
                l.number = base + k as u64;
                l.number_estimated = !exact;
            }
        }
        self.view = lines;
        self.apply_rows();
        self.view_dirty = false;
    }

    /// 把逻辑行展开成**完整**显示行列表（不切片），并返回视口最长行像素宽。
    /// 纯读操作，滚动定位时可以重复调用。
    fn build_rows(&self) -> (Vec<DisplayRow>, f64) {
        let cols = (self.text_width() / self.char_width.max(1.0)).max(8.0) as usize;
        let wrap = self.word_wrap;
        let mut rows = Vec::with_capacity(self.view.len() + 8);
        let mut max_px = 0.0f64;
        for (li, line) in self.view.iter().enumerate() {
            let width = display_cols(&line.text);
            max_px = max_px.max(self.display_px(&line.text));
            if !wrap || width <= cols {
                rows.push(DisplayRow {
                    logical: li,
                    sub: 0,
                    text: line.text.clone(),
                    byte_start: line.byte_start,
                    byte_len: line.byte_len,
                    number: line.number,
                    number_estimated: line.number_estimated,
                    truncated: line.truncated,
                });
                continue;
            }
            // 按显示列软折行：CJK / emoji 按 2 列计算，与等宽字体一致。
            // 用 char_indices 取边界，绝不会切在 UTF-8 字符中间。
            let mut sub = 0usize;
            let mut used = 0usize;
            let mut seg_start_byte = 0usize;
            for (i, ch) in line.text.char_indices() {
                let w = char_width_of(ch);
                if used + w > cols && used > 0 {
                    rows.push(make_row(line, li, sub, seg_start_byte, i));
                    sub += 1;
                    seg_start_byte = i;
                    used = 0;
                }
                used += w;
            }
            rows.push(make_row(line, li, sub, seg_start_byte, line.text.len()));
        }
        (rows, max_px)
    }

    /// 按 `top_sub` 跳过行首折行，再截断到屏幕真正放得下的显示行数。
    ///
    /// 这一步是本轮的关键：旧实现把 V 条逻辑行展开成 M>V 个显示行后
    /// 全塞进 `self.rows`，绘制只画前 V 个，剩下的（包含文件末尾）永远
    /// 不显示；`ensure_cursor_visible` 又拿 `rows.last()` 当视口末行，
    /// 光标可以跑到屏幕外。现在 `rows` 里保证全是可见显示行。
    fn apply_rows(&mut self) {
        let (all, max_px) = self.build_rows();
        self.max_line_px = max_px + 32.0;
        // 全局最长行只增不减：滚到短行区时水平范围不会突然收缩把 x_offset 拉回 0。
        self.doc_max_line_px = self.doc_max_line_px.max(self.max_line_px);

        let n0 = all.iter().filter(|r| r.logical == 0).count().max(1);
        self.first_line_rows = n0;
        if self.word_wrap && !self.view.is_empty() {
            let ratio = all.len() as f64 / self.view.len() as f64;
            if ratio > self.wrap_ratio {
                self.wrap_ratio = ratio.min(16.0);
            }
        }
        let skip = self.top_sub.min(n0.saturating_sub(1));
        self.top_sub = skip;
        self.rows = all.into_iter().skip(skip).take(self.visible_lines()).collect();
    }

    // ------------------------------------------------------------ 选区/编辑

    pub fn selection_range(&self) -> Option<(u64, u64)> {
        let a = self.sel_anchor?;
        let (lo, hi) = if a <= self.cursor { (a, self.cursor) } else { (self.cursor, a) };
        (lo != hi).then_some((lo, hi))
    }

    pub fn select_all(&mut self) {
        self.sel_anchor = Some(0);
        self.cursor = self.len();
        self.view_dirty = true;
    }

    pub fn selected_text(&mut self) -> String {
        let Some((s, e)) = self.selection_range() else { return String::new(); };
        // 剪贴板不需要搬运 GB 级文本，超过 8 MiB 时截断。
        let n = (e - s).min(8 * 1024 * 1024) as usize;
        let data = self.doc.as_mut().map(|d| d.read_range(s, n)).unwrap_or_default();
        String::from_utf8_lossy(&data).into_owned()
    }

    /// 插入文本（IME commit / 粘贴 / 换行 / ASCII 兜底都走这里）。
    pub fn insert_text(&mut self, text: &str) {
        if self.read_only {
            self.message = "只读模式，无法编辑（编辑 → 只读模式 可关闭）".to_string();
            return;
        }
        if text.is_empty() {
            return;
        }
        let (start, end) = self.selection_range().unwrap_or((self.cursor, self.cursor));
        let old_nl = self.newlines_in(start, end);
        let new_nl = text.bytes().filter(|&b| b == b'\n').count() as u64;
        let new_end = start + text.len() as u64;
        if let Some(doc) = self.doc.as_mut() {
            self.cursor = doc.replace(start, end, text.as_bytes());
        }
        self.apply_index_edit(start, end, new_end, old_nl, new_nl);
        self.sel_anchor = None;
        self.preedit.clear();
        self.preedit_cursor = 0;
        self.view_dirty = true;
        self.ensure_cursor_visible();
    }

    pub fn delete_range(&mut self, start: u64, end: u64) {
        if self.read_only || start >= end {
            return;
        }
        let old_nl = self.newlines_in(start, end);
        if let Some(doc) = self.doc.as_mut() {
            self.cursor = doc.replace(start, end, b"");
        }
        self.apply_index_edit(start, end, start, old_nl, 0);
        self.sel_anchor = None;
        self.preedit.clear();
        self.view_dirty = true;
        self.ensure_cursor_visible();
    }

    /// 撤销 / 重做：与正常输入共用 `apply_index_edit()` 增量维护行索引。
    ///
    /// 旧实现无条件 `self.line_index = None`，一次撤销会同时引发三件事：
    /// ① `total_lines()` 从精确值跌回 `len/80+1` 估算，滚动条 upper/page
    ///    剧变；② 紧接着的 `ensure_cursor_visible()` 用估算的
    ///    `byte_of_line()` 把视口拉到错误位置；③ 后台全文件索引重建的
    ///    每轮 tick 都重绘。快捷键能连按/长按，三者反复叠加就是「闪烁」。
    /// 现在索引始终精确：不重建、不跳位，一次撤销只重绘一次。
    pub fn undo_or_redo(&mut self, undo: bool) {
        let span = match self.doc.as_ref() {
            Some(d) => {
                if undo { d.peek_undo_span() } else { d.peek_redo_span() }
            }
            None => None,
        };
        let Some(span) = span else { return; };
        let old_end = span.start.saturating_add(span.old_len);
        let new_end = span.start.saturating_add(span.new_len);
        // 变更前：old 区间还是文档当前内容，此时数换行。
        let old_nl = self.newlines_in(span.start, old_end);
        let pos = match self.doc.as_mut() {
            Some(d) => {
                if undo { d.undo() } else { d.redo() }
            }
            None => return,
        };
        let Some(pos) = pos else { return; };
        // 变更后：new 区间已经是新内容。
        let new_nl = self.newlines_in(span.start, new_end).unwrap_or(0);
        self.apply_index_edit(span.start, old_end, new_end, old_nl, new_nl);
        self.cursor = pos;
        self.sel_anchor = None;
        self.preedit.clear();
        self.preedit_cursor = 0;
        self.view_dirty = true;
        self.ensure_cursor_visible();
    }

    pub fn prev_char_start(&mut self) -> u64 {
        if self.cursor == 0 {
            return 0;
        }
        let base = self.cursor.saturating_sub(16);
        let data = self.doc.as_mut().map(|d| d.read_range(base, 24)).unwrap_or_default();
        if data.is_empty() {
            return self.cursor.saturating_sub(1);
        }
        let rel = (self.cursor - base) as usize;
        let mut s = rel.min(data.len() - 1);
        while s > 0 && !utf8::is_char_boundary(data[s]) {
            s -= 1;
        }
        if s == 0 {
            return base;
        }
        let mut q = s - 1;
        while q > 0 && !utf8::is_char_boundary(data[q]) {
            q -= 1;
        }
        base + q as u64
    }

    pub fn next_char_end(&mut self) -> u64 {
        let len = self.len();
        if self.cursor >= len {
            return len;
        }
        let data = self.doc.as_mut().map(|d| d.read_range(self.cursor, 8)).unwrap_or_default();
        if data.is_empty() {
            return self.cursor;
        }
        self.cursor + utf8::char_len(data[0]).min(data.len()) as u64
    }

    pub fn backspace(&mut self) {
        if let Some((s, e)) = self.selection_range() {
            self.delete_range(s, e);
            return;
        }
        let p = self.prev_char_start();
        self.delete_range(p, self.cursor);
    }

    pub fn delete_forward(&mut self) {
        if let Some((s, e)) = self.selection_range() {
            self.delete_range(s, e);
            return;
        }
        let n = self.next_char_end();
        self.delete_range(self.cursor, n);
    }

    pub fn move_left(&mut self) {
        self.cursor = self.prev_char_start();
        self.sel_anchor = None;
        self.goal_col = 0;
        self.ensure_cursor_visible();
    }

    pub fn move_right(&mut self) {
        self.cursor = self.next_char_end();
        self.sel_anchor = None;
        self.goal_col = 0;
        self.ensure_cursor_visible();
    }

    /// 光标所在显示行的下标。
    pub fn current_row_index(&self) -> Option<usize> {
        let mut found = None;
        for (i, r) in self.rows.iter().enumerate() {
            if r.byte_start <= self.cursor {
                found = Some(i);
            } else {
                break;
            }
        }
        found
    }

    pub fn row_bounds(&self) -> Option<(u64, u64)> {
        let i = self.current_row_index()?;
        let r = &self.rows[i];
        Some((r.byte_start, r.byte_start + r.byte_len))
    }

    pub fn move_home(&mut self) {
        if let Some((s, _)) = self.row_bounds() {
            self.cursor = s;
        }
        self.sel_anchor = None;
        self.x_offset = 0.0;
    }

    pub fn move_end(&mut self) {
        if let Some((_, e)) = self.row_bounds() {
            self.cursor = e;
        }
        self.sel_anchor = None;
    }

    /// 按显示行上下移动，保持目标列。
    pub fn move_vertical(&mut self, delta: i32) {
        self.refresh_view();
        if self.rows.is_empty() {
            return;
        }
        let idx = self.current_row_index().unwrap_or(0) as i32;
        if self.goal_col == 0 {
            if let Some((s, _)) = self.row_bounds() {
                if let Some(r) = self.rows.get(idx as usize) {
                    let bytes = r.text.as_bytes();
                    let off = (self.cursor.saturating_sub(s) as usize).min(bytes.len());
                    self.goal_col = display_cols(&String::from_utf8_lossy(&bytes[..off]));
                }
            }
        }
        let goal = self.goal_col;
        let target = idx + delta;
        if target < 0 || target >= self.rows.len() as i32 {
            // 越界多少显示行就滚多少。旧实现按「一个逻辑行」滚，折行时
            // 一下跳过整条逻辑行的所有折行，选区/光标会骤然跨过一大段，
            // 用户反馈「按向下键可以选到很多」正来自这里。
            let need = if target < 0 {
                target
            } else {
                target - (self.rows.len() as i32 - 1)
            };
            self.scroll_by_display_rows(need);
        }
        self.refresh_view();
        let new_idx = (idx + delta).clamp(0, self.rows.len().saturating_sub(1) as i32) as usize;
        if let Some(r) = self.rows.get(new_idx) {
            let b = col_to_byte(&r.text, goal).min(r.text.len());
            self.cursor = r.byte_start + b as u64;
        }
        self.ensure_cursor_visible();
    }

    pub fn jump_to_line(&mut self, line: u64) {
        let total = self.total_lines();
        let line = line.min(total.saturating_sub(1));
        let max_top = self.scroll_max_top();
        self.top_line = line.saturating_sub(self.vertical_page() / 3).min(max_top);
        self.top_sub = 0;
        self.cursor = self.byte_of_line(line);
        self.sel_anchor = None;
        self.preedit.clear();
        self.view_dirty = true;
    }

    /// 切换自动换行：折行比与行内显示行偏移都必须重新学习/归零，
    /// 否则关掉换行后滚动条仍按旧比例算长度、视口起点还停在折行中间。
    pub fn set_word_wrap(&mut self, wrap: bool) {
        if self.word_wrap == wrap {
            return;
        }
        self.word_wrap = wrap;
        self.wrap_ratio = 1.0;
        self.top_sub = 0;
        self.x_offset = 0.0;
        self.top_line = self.top_line.min(self.scroll_max_top());
        self.view_dirty = true;
    }

    /// 视口宽度或字体变化时，折行数会变，旧的折行比不能再用来算滚动条长度。
    /// 由窗口层在 size_allocate / 缩放后调用；重置后随浏览重新学习（只增不减）。
    pub fn reset_wrap_learning(&mut self) {
        self.wrap_ratio = 1.0;
        // 保留 top_sub：apply_rows 会按新的折行数把它钳到合法范围，
        // 直接清零会让 resize 时视口起点跳回逻辑行首。
        self.view_dirty = true;
    }

    pub fn jump_to(&mut self, byte: u64) {
        let byte = byte.min(self.len());
        let line = self.line_of_byte(byte);
        self.jump_to_line(line);
        self.cursor = byte;
    }

    /// 光标进入视口所需的最小滚动。
    ///
    /// 旧实现有两个跳动源：① 用 byte 范围判越界，光标正好在最后一行行尾时
    /// 被误判，`top_line` 被拉走导致整屏跳；② 水平方向一次跳 `8 * char_width`，
    /// 每敲几个字整行就猛地左移一格。现在垂直只在真越界时动，水平小步跟随。
    pub fn ensure_cursor_visible(&mut self) {
        self.refresh_view();
        if self.rows.is_empty() {
            return;
        }
        let cur = self.cursor;
        let first = self.rows.first().map(|r| r.byte_start).unwrap_or(0);
        let last_row = self.rows.last().unwrap();
        let last = last_row.byte_start + last_row.byte_len;
        // +1 容差：末尾换行后的光标不算越界。
        if cur < first || cur > last.saturating_add(1) {
            let line = self.line_of_byte(cur);
            // 折行下视口容纳的逻辑行数比可见显示行数少，必须用 vertical_page()
            // 而不是 visible_lines()（后者会把 top_line 拉过头、末尾又看不到）。
            let visible = self.vertical_page().max(1);
            let max_top = self.scroll_max_top();
            self.top_line = if line < self.top_line {
                line.min(max_top)
            } else {
                // 与 scroll.rs 的 ROW_BUFFER 同一口径：视口尾部那几行只用于
                // 预取、画不出来，光标行不能落在那里（取消换行后尤其明显）。
                line.saturating_sub(visible.saturating_sub(owl_core::scroll::ROW_BUFFER))
                    .min(max_top)
            };
            self.top_sub = 0;
            self.view_dirty = true;
            self.refresh_view();
        }
        // 水平：只滚到刚好让光标露出，不整格跳跃（用 owl-core 的可测纯函数）。
        let (upper, page, _) = self.hscroll_params();
        self.x_offset = owl_core::scroll::follow_cursor_px(
            self.cursor_display_px(),
            self.x_offset,
            self.text_width(),
            self.char_width * 2.0,
            upper,
            page,
        );
    }

    /// 按逻辑行滚动（不换行或显式跳行时使用），回到逻辑行首。
    pub fn scroll_by_lines(&mut self, delta: i32) {
        let max_top = self.scroll_max_top();
        let t = self.top_line as i64 + delta as i64;
        self.top_line = t.clamp(0, max_top as i64) as u64;
        self.top_sub = 0;
        self.view_dirty = true;
    }

    /// 按**显示行**滚动。自动换行下必须走这里：按逻辑行滚一次会跳过
    /// 整条逻辑行的所有折行，中间的显示行永远看不见。
    ///
    /// 用当前视口已展开的显示行列表精确定位目标行；只有一次滚动超出
    /// 已加载范围时才按折行比保守推进（下一次滚动会继续逼近）。
    pub fn scroll_by_display_rows(&mut self, delta: i32) {
        if !self.word_wrap {
            self.scroll_by_lines(delta);
            return;
        }
        if delta == 0 {
            return;
        }
        self.refresh_view();
        let all = self.build_rows().0;
        if all.is_empty() {
            return;
        }
        let cur = self.top_sub.min(all.len() - 1);
        let logical: Vec<usize> = all.iter().map(|r| r.logical).collect();
        let subs: Vec<usize> = all.iter().map(|r| r.sub).collect();
        let (line_delta, new_sub, overshoot) =
            owl_core::scroll::step_display_rows(&logical, &subs, cur, delta);
        if overshoot == 0 {
            let max_top = self.scroll_max_top();
            self.top_line = (self.top_line + line_delta).min(max_top);
            self.top_sub = new_sub;
        } else if overshoot < 0 {
            // 往上越过当前逻辑行：退到上一个逻辑行的最后一个显示行。
            if self.top_line > 0 {
                self.top_line -= 1;
                // 由 apply_rows 钳到该行的最后一个子行。
                self.top_sub = 255;
            } else {
                self.top_sub = 0;
            }
        } else {
            // 往下越过已加载范围：按折行比估算跨过的逻辑行数。
            let extra = overshoot as usize;
            let step = (extra as f64 / self.wrap_ratio.max(1.0)).ceil().max(1.0) as u64;
            let loaded = all.last().map(|r| r.logical as u64 + 1).unwrap_or(1);
            let max_top = self.scroll_max_top();
            self.top_line = (self.top_line + loaded + step).min(max_top);
            self.top_sub = 0;
        }
        self.view_dirty = true;
    }

    pub fn scroll_to_line(&mut self, line: u64) {
        self.top_line = line.min(self.scroll_max_top());
        self.top_sub = 0;
        self.view_dirty = true;
    }

    // ------------------------------------------------------------ 坐标/状态

    fn cursor_display_col(&self) -> usize {
        let Some(i) = self.current_row_index() else { return 0; };
        let r = &self.rows[i];
        let off = (self.cursor.saturating_sub(r.byte_start)) as usize;
        let bytes = r.text.as_bytes();
        display_cols(&String::from_utf8_lossy(&bytes[..off.min(bytes.len())]))
    }

    /// 光标在本显示行内的**精确像素 x**（与 Pango 实测字符宽同源）。
    fn cursor_display_px(&self) -> f64 {
        let Some(i) = self.current_row_index() else { return 0.0; };
        let r = &self.rows[i];
        let off = (self.cursor.saturating_sub(r.byte_start)) as usize;
        let bytes = r.text.as_bytes();
        self.display_px(&String::from_utf8_lossy(&bytes[..off.min(bytes.len())]))
    }

    /// 光标在编辑器控件内的像素位置（已扣除水平滚动）。
    pub fn cursor_xy(&self) -> (i32, i32) {
        let mut row = 0usize;
        for (i, r) in self.rows.iter().enumerate() {
            if r.byte_start <= self.cursor {
                row = i;
            } else {
                break;
            }
        }
        // 用精确像素，不用 display_cols * char_width 的估算值。
        let x = (self.content_x() + self.cursor_display_px() - self.x_offset).round() as i32;
        let y = row as i32 * self.line_height;
        (x, y)
    }

    pub fn status_text(&mut self) -> String {
        let stats = self.doc.as_ref().map(|d| d.stats());
        let line = self.line_of_byte(self.cursor) + 1;
        let col = self.cursor_display_col() + 1;
        let total = self.total_lines();
        let sel = self.selection_range().map(|(a, b)| b - a).unwrap_or(0);
        let mut parts: Vec<String> = Vec::new();
        // 文件名放最前：一窗多标签时状态栏必须能看出「现在编辑的是谁」。
        match &self.path {
            Some(p) => {
                let raw = p.as_os_str().len();
                let name = p
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| p.display().to_string());
                // 路径太长时只留文件名 + 上级目录，避免把后面各项挤出去。
                if raw > 0 && name.len() + 1 < raw {
                    let parent = p
                        .parent()
                        .and_then(|q| q.file_name())
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    if parent.is_empty() {
                        parts.push(name);
                    } else {
                        parts.push(format!("{parent}/{name}"));
                    }
                } else {
                    parts.push(name);
                }
            }
            None => parts.push("未命名".to_string()),
        }
        parts.push(format!("行 {} / {}，列 {}", line, total, col));
        if sel > 0 {
            parts.push(format!("已选 {} 字节", sel));
        }
        if let Some(s) = &stats {
            parts.push(human_size(s.logical_len));
            parts.push(format!("内存 ≤ {:.1} MiB", s.heap_upper_bound as f64 / 1048576.0));
        }
        parts.push(self.encoding.clone());
        parts.push(self.line_ending.to_string());
        parts.push(format!("{}%", (self.font_size as f64 / 12.0 * 100.0) as i32));
        if self.read_only {
            parts.push("只读".to_string());
        }
        if self.word_wrap {
            parts.push("自动换行".to_string());
        }
        if self.index_building || !self.index_exact() {
            parts.push(format!("索引 {:.0}%", self.index_progress * 100.0));
        }
        match &stats {
            Some(s) if s.dirty => parts.push("● 已修改".to_string()),
            // 未命名缓冲区没有落盘路径，「已保存」会误导成文件已存在。
            Some(_) if self.path.is_none() => parts.push("未保存（未命名）".to_string()),
            Some(_) => parts.push("已保存".to_string()),
            None => parts.push("未打开文件".to_string()),
        }
        let base = parts.join("   │   ");
        if self.message.is_empty() {
            base
        } else {
            format!("{}   │   {}", base, self.message)
        }
    }

    /// 输入法索取光标上下文。
    pub fn surrounding_text(&mut self) -> (String, i32, i32) {
        let Some(doc) = self.doc.as_mut() else { return (String::new(), 0, 0); };
        let back = self.cursor.min(256);
        let from = self.cursor - back;
        let data = doc.read_range(from, 512);
        let s = utf8::prev_boundary(&data, back as usize);
        let e = utf8::next_boundary(&data, (back + 256).min(data.len() as u64) as usize);
        let text = String::from_utf8_lossy(&data[s..e]).into_owned();
        let cursor_idx = (back as usize - s) as i32;
        let anchor_idx = match self.sel_anchor {
            Some(a) if a >= from && a <= from + e as u64 => (a - from) as i32 - s as i32,
            _ => cursor_idx,
        };
        (text, cursor_idx, anchor_idx)
    }

    pub fn delete_surrounding_chars(&mut self, offset: i32, n: i32) {
        if n <= 0 {
            return;
        }
        let range = {
            let Some(doc) = self.doc.as_mut() else { return; };
            let back = self.cursor.min(256);
            let from = self.cursor - back;
            let data = doc.read_range(from, 512);
            let cur = back as usize;
            let mut bounds: Vec<usize> = (0..data.len())
                .filter(|&i| utf8::is_char_boundary(data[i]))
                .collect();
            bounds.push(data.len());
            let ci = bounds.partition_point(|&b| b <= cur).saturating_sub(1);
            let a = (ci as i64 + offset as i64).clamp(0, bounds.len() as i64 - 1) as usize;
            let b = (a as i64 + n as i64).clamp(0, bounds.len() as i64 - 1) as usize;
            let (lo, hi) = (a.min(b), a.max(b));
            (from + bounds[lo] as u64, from + bounds[hi] as u64)
        };
        self.delete_range(range.0, range.1);
    }

    /// 垂直滚动条允许的最大 `top_line`。GTK Adjustment 的合法 value 上界是
    /// `upper - page_size`，状态必须遵守同一口径，否则 `set_value` 会被 GTK
    /// clamp，两边反复拉扯，拖拽时表现为「滑不动 / 弹回」。
    pub fn scroll_max_top(&self) -> u64 {
        if self.word_wrap {
            owl_core::scroll::max_top_wrapped(self.total_lines())
        } else {
            owl_core::scroll::max_top(self.total_lines(), self.visible_lines())
        }
    }

    /// 垂直滚动条的 page_size（等效逻辑行数），折行越严重越小。
    pub fn vertical_page(&self) -> u64 {
        if self.word_wrap {
            owl_core::scroll::wrapped_page(self.visible_lines(), self.wrap_ratio) as u64
        } else {
            self.visible_lines() as u64
        }
    }

    pub fn scroll_params(&self) -> (f64, f64, f64) {
        let p = if self.word_wrap {
            owl_core::scroll::vertical_wrapped(
                self.total_lines(),
                self.visible_lines(),
                self.wrap_ratio,
                self.top_line,
                self.top_sub,
                self.first_line_rows,
            )
        } else {
            owl_core::scroll::vertical(
                self.total_lines(),
                self.visible_lines(),
                self.top_line,
            )
        };
        (p.upper, p.page, p.value)
    }

    pub fn hscroll_params(&self) -> (f64, f64, f64) {
        // 用「打开以来见过的全局最长行」而不是只看当前可见的几十行：
        // 后者会在滚到短行区域时把 upper 缩回 page，横向滚动条突然失效、
        // x_offset 被硬拉回 0。
        let longest = self.max_line_px.max(self.doc_max_line_px);
        // 换行开启时文字已折到视口宽度内，没有横向溢出，水平滚动条应
        // 直接失效并隐藏（owl_core::scroll::horizontal 在 wrap=true 时返回
        // upper==page，使滑块不可拖）；关掉换行后才需要精确的长行横向范围。
        // 上一版把 wrap 写死成 false，导致「自动换行下横向条仍是没换行时
        // 的长行范围」——这是用户反馈的根因。
        let p = owl_core::scroll::horizontal(
            longest,
            self.text_width(),
            self.x_offset,
            self.word_wrap,
        );
        (p.upper, p.page, p.value)
    }
}

fn make_row(line: &RenderLine, li: usize, sub: usize, s: usize, e: usize) -> DisplayRow {
    DisplayRow {
        logical: li,
        sub,
        text: line.text[s..e].to_string(),
        byte_start: line.byte_start + s as u64,
        byte_len: (e - s) as u64,
        number: line.number,
        number_estimated: line.number_estimated,
        truncated: line.truncated,
    }
}

fn char_width_of(c: char) -> usize {
    // 等宽字体下 CJK / emoji 约占两列；控制字符按 0 处理。
    if c.is_control() {
        0
    } else if c.len_utf8() > 1 {
        2
    } else {
        1
    }
}

pub fn display_cols(s: &str) -> usize {
    s.chars().map(char_width_of).sum()
}

/// 显示列 -> 字节列。
fn col_to_byte(s: &str, target: usize) -> usize {
    let mut used = 0usize;
    for (i, c) in s.char_indices() {
        if used >= target {
            return i;
        }
        used += char_width_of(c);
    }
    s.len()
}

pub fn human_size(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{} {}", n, UNITS[i])
    } else {
        format!("{:.2} {}", v, UNITS[i])
    }
}
