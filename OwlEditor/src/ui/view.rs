//! 虚拟自绘文本视图。
//!
//! GTK 自带 `GtkTextView` 会把整个 buffer 放进内存，无法承载 10 GiB。
//! 这里继承 `GtkWidget` 实现 `snapshot()`，只绘制当前视口的几十行。
//! 视口由 `AppState::top_line`（精确行号）驱动，行索引建好后不会有重复行。

use crate::ui::ime;
use crate::ui::state::{with_state, AppState, Shared};
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene, pango};
use owl_core::{compose_preedit, hit};
use std::cell::{Cell, OnceCell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct EditorView {
        pub state: OnceCell<Shared>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for EditorView {
        const NAME: &'static str = "OwlEditorView";
        type Type = super::EditorView;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for EditorView {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            obj.set_focusable(true);
            obj.set_can_focus(true);
            obj.set_hexpand(true);
            obj.set_vexpand(true);
            obj.set_overflow(gtk::Overflow::Hidden);
            obj.add_css_class("owl-editor");
        }
    }

    impl WidgetImpl for EditorView {
        fn measure(&self, _o: gtk::Orientation, _s: i32) -> (i32, i32, i32, i32) {
            (360, 480, -1, -1)
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            self.parent_size_allocate(width, height, baseline);
            if let Some(state) = self.state.get() {
                let obj = self.obj();
                let mut st = state.borrow_mut();
                let size_changed = st.width != width || st.height != height;
                st.width = width;
                st.height = height;
                // 字体度量已缓存：尺寸变化不再重算 approximate_char_width，
                // 避免每次重排都让行号槽与正文 x 抖动。
                st.update_metrics(&*obj);
                if size_changed {
                    st.view_dirty = true;
                    st.doc_max_line_px = st.doc_max_line_px.max(st.max_line_px);
                    // 视口宽度变了，每行折成几行也变了，滚动条长度必须重新学习。
                    st.reset_wrap_learning();
                }
                drop(st);
                // 只请求重绘，不在 size_allocate 里回写 adjustment 或触发 hook，
                // 否则会与 GTK 的布局循环互相触发。
                if size_changed {
                    obj.queue_draw();
                }
            }
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            if let Some(state) = self.state.get() {
                render(&self.obj(), snapshot, state);
            }
        }
    }
}

glib::wrapper! {
    pub struct EditorView(ObjectSubclass<imp::EditorView>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl EditorView {
    pub fn new(state: &Shared) -> Self {
        let obj: Self = glib::Object::builder().build();
        let _ = obj.imp().state.set(state.clone());
        setup_controllers(&obj, state);
        obj
    }
}

fn rect(x: f32, y: f32, w: f32, h: f32) -> graphene::Rect {
    graphene::Rect::new(x, y, w.max(0.0), h.max(0.0))
}

fn x_for_byte(layout: &pango::Layout, byte: usize) -> f64 {
    layout.index_to_pos(byte as i32).x() as f64 / pango::SCALE as f64
}

fn render(widget: &EditorView, snapshot: &gtk::Snapshot, state: &Shared) {
    let w = widget.width() as f32;
    let h = widget.height() as f32;
    let mut st = state.borrow_mut();
    st.refresh_view();

    let theme = st.theme.clone();
    let font = st.font.clone();
    let content_x = st.content_x() as f32;
    let gutter = if st.show_line_numbers { st.gutter_width as f32 } else { 0.0 };
    let text_w = (w - content_x).max(0.0);
    let lh = st.line_height as f32;
    let char_w = st.char_width as f32;
    let x_off = st.x_offset as f32;
    let selection = st.selection_range();
    let cursor = st.cursor;
    let cursor_row = st.current_row_index();
    let show_numbers = st.show_line_numbers;
    let preedit = st.preedit.clone();
    let preedit_cursor = st.preedit_cursor;
    let rows = st.rows.clone();
    let hits = st.hits.clone();
    // 预编辑串属于光标所在的显示行。
    let preedit_row = cursor_row;
    // 自动换行时，同一逻辑行的所有显示行都算「当前行」。
    let cursor_logical = cursor_row.and_then(|i| rows.get(i).map(|r| r.logical));

    snapshot.append_color(&theme.bg, &rect(0.0, 0.0, w, h));
    if show_numbers {
        snapshot.append_color(&theme.gutter_bg, &rect(0.0, 0.0, gutter, h));
    }

    // 正文裁剪：横向滚动后长行不能溢出到行号槽里。
    snapshot.push_clip(&rect(content_x, 0.0, text_w, h));
    // 预编辑期间光标应停在预编辑串内部的 caret 位置，而不是插入点。
    let mut caret_x_override: Option<f32> = None;
    for (i, row) in rows.iter().enumerate() {
        let y = i as f32 * lh;
        if y > h {
            break;
        }
        let row_lo = row.byte_start;
        let row_hi = row.byte_start + row.byte_len;

        if cursor_logical == Some(row.logical)
            || (cursor_logical.is_none() && cursor >= row_lo && cursor <= row_hi)
        {
            snapshot.append_color(&theme.current_line, &rect(0.0, y, w, lh));
        }

        // 预编辑串拼进本行显示文本：后续字符由 Pango 自然右移，不再叠字。
        // 文字用正文色 `theme.fg`（白色），与已提交文字完全一致。
        let composed = (preedit_row == Some(i) && !preedit.is_empty()).then(|| {
            compose_preedit(
                &row.text,
                cursor.saturating_sub(row_lo) as usize,
                &preedit,
                preedit_cursor,
            )
        });
        let display_text = composed.as_ref().map(|c| c.text.as_str()).unwrap_or(&row.text);
        let layout = widget.create_pango_layout(Some(display_text));
        layout.set_font_description(Some(&font));
        let map = |b: usize| composed.as_ref().map(|c| c.to_layout_byte(b)).unwrap_or(b);

        for hit in hits.iter().filter(|hit| hit.end > row_lo && hit.start < row_hi) {
            let a = (hit.start.max(row_lo) - row_lo) as usize;
            let b = (hit.end.min(row_hi) - row_lo) as usize;
            let x1 = x_for_byte(&layout, map(a)) as f32;
            let x2 = x_for_byte(&layout, map(b)) as f32;
            snapshot.append_color(
                &theme.match_hl,
                &rect(content_x - x_off + x1, y, (x2 - x1).max(char_w), lh),
            );
        }

        if let Some((a, b)) = selection {
            let lo = a.max(row_lo);
            let hi = b.min(row_hi);
            if lo < hi {
                let x1 = x_for_byte(&layout, map((lo - row_lo) as usize)) as f32;
                let x2 = x_for_byte(&layout, map((hi - row_lo) as usize)) as f32;
                snapshot.append_color(
                    &theme.selection,
                    &rect(content_x - x_off + x1, y, (x2 - x1).max(1.0), lh),
                );
            }
        }

        snapshot.save();
        snapshot.translate(&graphene::Point::new(content_x - x_off, y));
        snapshot.append_layout(&layout, &theme.fg);
        snapshot.restore();

        if let Some(c) = &composed {
            // 低对比度灰色细线标出「尚未提交」的区间，不再是蓝色。
            let u1 = x_for_byte(&layout, c.off) as f32;
            let u2 = x_for_byte(&layout, c.off + c.shift) as f32;
            snapshot.append_color(
                &theme.preedit_underline,
                &rect(content_x - x_off + u1, y + lh - 2.0, (u2 - u1).max(1.0), 1.0),
            );
            caret_x_override = Some(
                content_x - x_off + x_for_byte(&layout, c.caret_byte()) as f32,
            );
        }
    }
    snapshot.pop();

    // 行号（只在逻辑行的第一个显示行画）
    if show_numbers {
        snapshot.push_clip(&rect(0.0, 0.0, gutter, h));
        for (i, row) in rows.iter().enumerate() {
            let y = i as f32 * lh;
            if y > h {
                break;
            }
            let num = if row.sub > 0 {
                String::new()
            } else if row.number_estimated {
                format!("~{}", row.number + 1)
            } else {
                format!("{}", row.number + 1)
            };
            if num.is_empty() {
                continue;
            }
            let nl = widget.create_pango_layout(Some(&num));
            nl.set_font_description(Some(&font));
            nl.set_alignment(pango::Alignment::Right);
            nl.set_width((gutter as i32 - 12).max(1) * pango::SCALE);
            snapshot.save();
            snapshot.translate(&graphene::Point::new(0.0, y));
            snapshot.append_layout(&nl, &theme.gutter_fg);
            snapshot.restore();
        }
        snapshot.pop();
    }

    // 光标：预编辑期间停在预编辑串内部的 caret 处，别再压在串首。
    if st.has_doc() {
        let (cx, cy) = st.cursor_xy();
        let px = caret_x_override.unwrap_or(cx as f32);
        if px >= content_x as f32 - 2.0 && px <= w as f32 && cy >= 0 && cy <= h as i32 {
            snapshot.append_color(&theme.cursor, &rect(px, cy as f32, 2.0, lh));
        }
    }
    drop(st);
}

/// 把窗口像素坐标换算成逻辑字节偏移。
fn byte_at_point(widget: &EditorView, state: &Shared, x: f64, y: f64) -> Option<u64> {
    let mut s = state.borrow_mut();
    s.refresh_view();
    let row_idx = (y / s.line_height as f64).max(0.0) as usize;
    let row = s.rows.get(row_idx)?.clone();
    // 预编辑期间显示文本里插了未提交串，命中测试必须用同一份合成文本，
    // 再把结果映射回真实字节偏移，否则点一下光标就会跳过整个拼音串。
    let composed = (s.current_row_index() == Some(row_idx) && !s.preedit.is_empty()).then(|| {
        compose_preedit(
            &row.text,
            s.cursor.saturating_sub(row.byte_start) as usize,
            &s.preedit,
            s.preedit_cursor,
        )
    });
    let display_text = composed.as_ref().map(|c| c.text.as_str()).unwrap_or(&row.text);
    let layout = widget.create_pango_layout(Some(display_text));
    layout.set_font_description(Some(&s.font));
    let px = ((x - s.content_x() + s.x_offset).max(0.0) * pango::SCALE as f64) as i32;
    let (_inside, index, trailing) = layout.xy_to_index(px, 0);
    // Pango 命中行末时返回「最后一个字符的起始索引 + trailing=1」，旧实现
    // 丢掉 trailing，光标就永远停在最后一个字符之前（用户反馈「永远不在最后」）。
    // 点在整行右侧空白时还要额外判定越过文本右边界，直接落到行尾。
    let text_px = layout.pixel_size().0 as f64 * pango::SCALE as f64;
    let past_end = px as f64 > text_px + pango::SCALE as f64 * 0.5;
    let mut idx = hit::hit_byte(display_text, index, trailing, past_end);
    if let Some(c) = &composed {
        idx = c.from_layout_byte(idx);
    }
    Some(row.byte_start + (idx as u64).min(row.byte_len))
}

/// 方向键/翻页键统一入口：Shift 时保留（或新建）选区锚点，非 Shift 时清除。
/// 旧实现里 move_left/right/home/end 自己清 anchor、move_vertical 不清，
/// 导致 Shift+← 完全没反应、Shift+↓ 却「碰巧」能扩展，行为不一致。
fn navigate(state: &Shared, shift: bool, f: impl FnOnce(&mut AppState)) {
    with_state(state, |st| {
        let anchor = st.sel_anchor;
        let old = st.cursor;
        f(st);
        if shift {
            st.sel_anchor = Some(anchor.unwrap_or(old));
        } else {
            st.sel_anchor = None;
        }
    });
}

/// Ctrl 组合键 -> 统一动作名。菜单、工具栏、快捷键共用一个分发口。
fn ctrl_action(key: gdk::Key, shift: bool) -> Option<&'static str> {
    use gdk::Key as K;
    Some(match key {
        // Ctrl+N 新建标签页，Ctrl+Shift+N 新建窗口（Kate 习惯）。
        K::n if shift => "new_window",
        K::n => "new",
        K::o => "open",
        K::s if shift => "save_as",
        K::s => "save",
        K::w => "close",
        K::q => "quit",
        K::f => "find",
        K::h | K::r => "replace",
        K::g => "goto",
        K::p => "print",
        K::a => "select_all",
        K::z if shift => "redo",
        K::z => "undo",
        K::y => "redo",
        K::c => "copy",
        K::v => "paste",
        K::x => "cut",
        K::d => "delete_line",
        K::l if shift => "duplicate_line",
        K::e => "word_count",
        K::equal | K::plus | K::KP_Add => "zoom_in",
        K::minus | K::KP_Subtract => "zoom_out",
        K::_0 => "zoom_reset",
        K::Home => "doc_start",
        K::End => "doc_end",
        K::F3 => "find_next",
        _ => return None,
    })
}

fn setup_controllers(widget: &EditorView, state: &Shared) {
    let bundle = ime::attach(widget, state);
    ime::log_backend(&bundle.im);

    // 键盘自动重复过滤：Ctrl 组合的一次性动作（撤销 / 重做 / 保存 / 查找…）
    // 被长按会以 ~30Hz 连续触发，每次都走一次完整重绘，视觉上就是「闪烁」。
    // 普通字符输入与方向键的自动重复必须保留，所以只在 Ctrl 动作分支使用。
    // key-released 正常到达时立即清除；万一丢失（切窗口、IME 吞事件），
    // 900ms 的时间兜底也会让下一次按键重新算作新按下。
    let pressed: Rc<RefCell<HashMap<u32, Instant>>> = Rc::new(RefCell::new(HashMap::new()));
    {
        let pressed = pressed.clone();
        bundle.key.connect_key_released(move |_, _keyval, keycode, _mods| {
            pressed.borrow_mut().remove(&keycode);
        });
    }

    // ---- 键盘 ----
    let s = state.clone();
    let view = widget.clone();
    bundle.key.connect_key_pressed(move |_, keyval, keycode, mods| {
        let is_repeat = {
            let mut m = pressed.borrow_mut();
            let now = Instant::now();
            let repeat = m
                .get(&keycode)
                .map(|t| now.duration_since(*t) < Duration::from_millis(900))
                .unwrap_or(false);
            m.insert(keycode, now);
            repeat
        };
        let ctrl = mods.contains(gdk::ModifierType::CONTROL_MASK);
        let shift = mods.contains(gdk::ModifierType::SHIFT_MASK);
        let alt = mods.contains(gdk::ModifierType::ALT_MASK);

        // Ctrl+方向键 / Home / End / 翻页键没有绑定菜单动作，旧实现在这里
        // 无条件 Stop，按键被整个吞掉（用户按 Ctrl+↓ 毫无反应）。放行给
        // 下面的导航逻辑，让它们等价于对应的普通方向键。
        let nav_key = matches!(
            keyval,
            gdk::Key::Up
                | gdk::Key::Down
                | gdk::Key::Left
                | gdk::Key::Right
                | gdk::Key::Home
                | gdk::Key::End
                | gdk::Key::Page_Up
                | gdk::Key::Page_Down
        );
        // Ctrl+Tab / Ctrl+PageUp / Ctrl+PageDown：切换标签页。
        // Page 键在 nav_key 白名单里，必须抢在导航分支之前处理，否则会变成翻页。
        let tab_key = matches!(
            keyval,
            gdk::Key::Tab | gdk::Key::ISO_Left_Tab | gdk::Key::Page_Up | gdk::Key::Page_Down
        );
        if ctrl && !alt && tab_key {
            if !is_repeat {
                let forward = match keyval {
                    gdk::Key::Page_Down => true,
                    gdk::Key::Page_Up => false,
                    _ => !shift,
                };
                let hook = { s.borrow().on_action.clone() };
                if let Some(h) = hook {
                    h(if forward { "next_tab" } else { "prev_tab" });
                }
            }
            return glib::Propagation::Stop;
        }
        if ctrl && !alt && !nav_key {
            // 自动重复只触发第一次：撤销/重做长按不再变成连续重绘。
            if !is_repeat {
                if let Some(name) = ctrl_action(keyval, shift) {
                    let hook = { s.borrow().on_action.clone() };
                    if let Some(h) = hook {
                        h(name);
                    }
                }
            }
            return glib::Propagation::Stop;
        }
        if ctrl && (!nav_key || alt) {
            return glib::Propagation::Proceed;
        }

        // 功能键
        if keyval == gdk::Key::F3 {
            let hook = { s.borrow().on_action.clone() };
            if let Some(h) = hook {
                h(if shift { "find_prev" } else { "find_next" });
            }
            return glib::Propagation::Stop;
        }
        if keyval == gdk::Key::F5 {
            let hook = { s.borrow().on_action.clone() };
            if let Some(h) = hook {
                h("reload");
            }
            return glib::Propagation::Stop;
        }
        if keyval == gdk::Key::F11 {
            let hook = { s.borrow().on_action.clone() };
            if let Some(h) = hook {
                h("fullscreen");
            }
            return glib::Propagation::Stop;
        }

        let handled = if keyval == gdk::Key::Left {
            navigate(&s, shift, |st| st.move_left());
            true
        } else if keyval == gdk::Key::Right {
            navigate(&s, shift, |st| st.move_right());
            true
        } else if keyval == gdk::Key::Up {
            navigate(&s, shift, |st| st.move_vertical(-1));
            true
        } else if keyval == gdk::Key::Down {
            navigate(&s, shift, |st| st.move_vertical(1));
            true
        } else if keyval == gdk::Key::Home {
            navigate(&s, shift, |st| st.move_home());
            true
        } else if keyval == gdk::Key::End {
            navigate(&s, shift, |st| st.move_end());
            true
        } else if keyval == gdk::Key::Page_Up {
            navigate(&s, shift, |st| {
                let n = st.visible_lines() as i32;
                st.move_vertical(-n);
            });
            true
        } else if keyval == gdk::Key::Page_Down {
            navigate(&s, shift, |st| {
                let n = st.visible_lines() as i32;
                st.move_vertical(n);
            });
            true
        } else if keyval == gdk::Key::BackSpace {
            with_state(&s, |st| st.backspace());
            true
        } else if keyval == gdk::Key::Delete {
            with_state(&s, |st| st.delete_forward());
            true
        } else if keyval == gdk::Key::Return || keyval == gdk::Key::KP_Enter {
            with_state(&s, |st| st.insert_text("\n"));
            true
        } else if keyval == gdk::Key::Tab {
            with_state(&s, |st| st.insert_text("    "));
            true
        } else if keyval == gdk::Key::Escape {
            with_state(&s, |st| {
                st.sel_anchor = None;
                st.preedit.clear();
            });
            true
        } else {
            false
        };

        if handled {
            ime::update_cursor_location(&s, &view);
            return glib::Propagation::Stop;
        }

        // 兜底：IME 未消费的可打印字符（set_im_context 保证两条路径互斥），
        // 这样即使会话没有输入法也能正常打字。
        if !alt {
            if let Some(ch) = keyval.to_unicode() {
                if !ch.is_control() {
                    let text = ch.to_string();
                    with_state(&s, |st| st.insert_text(&text));
                    ime::update_cursor_location(&s, &view);
                    return glib::Propagation::Stop;
                }
            }
        }
        glib::Propagation::Proceed
    });

    // ---- 滚轮：垂直按行、水平按像素 ----
    let s = state.clone();
    let view = widget.clone();
    let scroll = gtk::EventControllerScroll::new(
        gtk::EventControllerScrollFlags::VERTICAL
            | gtk::EventControllerScrollFlags::HORIZONTAL,
    );
    scroll.connect_scroll(move |_, dx, dy| {
        with_state(&s, |st| {
            if dx != 0.0 {
                let (upper, page, _) = st.hscroll_params();
                let max = (upper - page).max(0.0);
                st.x_offset = (st.x_offset + dx * st.char_width * 3.0).clamp(0.0, max);
            }
            if dy != 0.0 {
                // 自动换行下按显示行滚，不能按逻辑行（会跳过整段折行）。
                st.scroll_by_display_rows((dy * 3.0) as i32);
            }
        });
        ime::update_cursor_location(&s, &view);
        glib::Propagation::Stop
    });
    widget.add_controller(scroll);

    // ---- 鼠标：点击定位、拖拽选择 ----
    let dragging = Rc::new(Cell::new(false));
    let click = gtk::GestureClick::new();
    let s = state.clone();
    let view = widget.clone();
    let d1 = dragging.clone();
    click.connect_pressed(move |_, n_press, x, y| {
        if let Some(byte) = byte_at_point(&view, &s, x, y) {
            with_state(&s, |st| {
                if n_press == 2 {
                    // 双击选词
                    let (a, b) = word_at(st, byte);
                    st.sel_anchor = Some(a);
                    st.cursor = b;
                } else {
                    st.sel_anchor = Some(byte);
                    st.cursor = byte;
                }
                st.view_dirty = true;
            });
            d1.set(true);
            view.grab_focus();
        }
    });
    let d2 = dragging.clone();
    click.connect_released(move |_, _n, _x, _y| d2.set(false));
    widget.add_controller(click);

    let motion = gtk::EventControllerMotion::new();
    let s = state.clone();
    let view = widget.clone();
    motion.connect_motion(move |_, x, y| {
        if !dragging.get() {
            return;
        }
        if let Some(byte) = byte_at_point(&view, &s, x, y) {
            with_state(&s, |st| {
                st.cursor = byte;
                st.view_dirty = true;
            });
        }
    });
    widget.add_controller(motion);
}

/// 双击选词：向左右扩展到非单词字符。
fn word_at(st: &mut crate::ui::state::AppState, byte: u64) -> (u64, u64) {
    let from = byte.saturating_sub(256);
    let data = st.doc.as_mut().map(|d| d.read_range(from, 512)).unwrap_or_default();
    let rel = (byte - from) as usize;
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80;
    let mut a = rel.min(data.len());
    let mut b = a;
    while a > 0 && is_word(data[a - 1]) {
        a -= 1;
    }
    while b < data.len() && is_word(data[b]) {
        b += 1;
    }
    (from + a as u64, from + b as u64)
}

/// 供窗口层设置刷新钩子后做一次初始度量。
pub fn initial_metrics(widget: &EditorView, state: &Shared) {
    let mut s = state.borrow_mut();
    s.update_metrics(widget);
    drop(s);
    ime::update_cursor_location(state, widget);
}
