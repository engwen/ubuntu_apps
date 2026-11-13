//! Wayland 原生中文输入。
//!
//! ## 为什么这里不需要任何环境变量 hack
//!
//! GTK4 在 Wayland 会话里让 `GtkIMMulticontext` 默认选择内置的
//! `GtkIMContextWayland`，它直接实现 `zwp_text_input_v3`。compositor
//! （GNOME / Mutter、Sway 等）作为中间人把 ibus-rime 的预编辑、候选与
//! commit 通过 text-input-v3 送进应用。链路是：
//!
//! ```text
//! ibus-rime ⟷ compositor ⟷ zwp_text_input_v3 ⟷ GtkIMContextWayland
//!          ⟷ GtkIMMulticontext ⟷ EventControllerKey ⟷ 自绘编辑器
//! ```
//!
//! Notepad-- 是 Qt5 应用，需要 `ibus-qt5` / `fcitx-frontend-qt5` 桥接插件，
//! 甚至要显式设置 `QT_IM_MODULE`；这里**一个 setenv 都没有**。
//! 代码只做三件事（也是 GTK 官方 "Text input in GTK 4" 给出的方式）：
//!
//! 1. `EventControllerKey::set_im_context(im)` —— 按键先交给输入法，
//!    被消费的键不再到达我们的 `key-pressed`，天然不会把拼音字母当文本插入。
//! 2. 接 `::commit` 拿最终文本（中文从这里进来）、`::preedit-*`
//!    在光标处绘制预编辑串。
//! 3. `set_client_widget` + `focus_in/out` + `set_cursor_location` +
//!    `retrieve-surrounding` —— 让候选框跟随光标、让输入法能读上下文。
//!
//! 注意：自绘 Widget 默认**不接收**任何输入法事件，必须显式接上
//! IMMulticontext。这正是「大文件虚拟绘制」与「输入法」的核心矛盾，
//! 也是很多大文件编辑器中文输入失效的原因。

use crate::ui::state::{with_state, Shared};
use gtk::prelude::*;
use gtk::gdk;

pub struct ImeBundle {
    pub im: gtk::IMMulticontext,
    pub key: gtk::EventControllerKey,
}

/// 把输入法接到自绘编辑器控件上。
pub fn attach(widget: &impl IsA<gtk::Widget>, state: &Shared) -> ImeBundle {
    let im = gtk::IMMulticontext::new();
    im.set_use_preedit(true);
    im.set_input_purpose(gtk::InputPurpose::FreeForm);
    im.set_client_widget(Some(widget));

    let key = gtk::EventControllerKey::new();
    // 关键：按键先过输入法。返回 true 的按键不会再触发 key-pressed。
    key.set_im_context(Some(&im));

    // ---- commit：这里收到的可能是中文、emoji 或普通 ASCII ----
    let s = state.clone();
    im.connect_commit(move |_, text| {
        if !text.is_empty() {
            with_state(&s, |st| st.insert_text(text));
        }
    });

    // ---- 预编辑（拼音串 / 候选高亮）----
    let s = state.clone();
    im.connect_preedit_changed(move |ctx| {
        let (text, _attrs, cursor_pos) = ctx.preedit_string();
        with_state(&s, |st| {
            st.preedit = text.to_string();
            st.preedit_cursor = cursor_pos;
        });
    });
    let s = state.clone();
    im.connect_preedit_start(move |_| {
        with_state(&s, |st| st.preedit.clear());
    });
    let s = state.clone();
    im.connect_preedit_end(move |_| {
        with_state(&s, |st| {
            st.preedit.clear();
            st.preedit_cursor = 0;
        });
    });

    // ---- 输入法要求删除光标周围字符（如重选候选）----
    let s = state.clone();
    im.connect_delete_surrounding(move |_, offset, n_chars| {
        with_state(&s, |st| st.delete_surrounding_chars(offset, n_chars));
        true
    });

    // ---- 输入法索取上下文 ----
    let s = state.clone();
    im.connect_retrieve_surrounding(move |ctx| {
        let (text, cursor_idx, anchor_idx) = {
            let mut st = s.borrow_mut();
            st.surrounding_text()
        };
        ctx.set_surrounding_with_selection(&text, cursor_idx, anchor_idx);
        true
    });

    // ---- 焦点 ----
    let focus = gtk::EventControllerFocus::new();
    let im_in = im.clone();
    focus.connect_enter(move |_| im_in.focus_in());
    let im_out = im.clone();
    focus.connect_leave(move |_| im_out.focus_out());
    widget.add_controller(focus);
    widget.add_controller(key.clone());

    state.borrow_mut().im = Some(im.clone());
    ImeBundle { im, key }
}

/// 光标移动 / 视口滚动后通知输入法，让候选框贴在光标下方。
pub fn update_cursor_location(state: &Shared, widget: &impl IsA<gtk::Widget>) {
    let _ = widget;
    let (rect, im) = {
        let mut s = state.borrow_mut();
        s.refresh_view();
        let (x, y) = s.cursor_xy();
        let h = s.line_height;
        let im = s.im.clone();
        (gdk::Rectangle::new(x, y, 1, h), im)
    };
    if let Some(im) = im {
        im.set_cursor_location(&rect);
    }
}

/// 内容被程序改动（跳转、撤销、选择）后清掉未完成的预编辑。
pub fn reset(state: &Shared) {
    let im = state.borrow().im.clone();
    if let Some(im) = im {
        im.reset();
    }
}

/// 调试用：打印实际生效的 IM delegate，便于在真机上确认走的是 wayland。
/// 不设置 `context-id`，保持 GTK4 的默认选择——Wayland 下即
/// `GtkIMContextWayland` / `zwp_text_input_v3`。
pub fn log_backend(im: &gtk::IMMulticontext) {
    println!(
        "[OwlEditor] active IM delegate = {:?} (GTK4 default; Wayland -> text-input-v3)",
        im.context_id().as_str()
    );
}
