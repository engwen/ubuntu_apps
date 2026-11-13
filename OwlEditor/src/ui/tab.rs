//! 单个文档标签页。
//!
//! 早期版本是「一个窗口 = 一个文档」，菜单、查找栏、滚动条、状态栏和唯一
//! 那份 `AppState` 全焊在 `window.rs` 里。要支持「一个窗口打开多个文件」，
//! 就得把文档相关的一切抽出来：本文件把一份文档所需的状态、自绘视图、
//! 查找/替换栏、双向滚动条、状态栏与动作分发封装成一个 [`Tab`]；窗口层
//! 只负责标签栏、菜单和跨标签动作（新建 / 打开 / 关闭 / 换主题）。
//!
//! 未命名缓冲区：新建标签页不是「先落一个临时文件」，而是
//! `Document::untitled()`（匿名 fd），所以没打开文件也能直接打字；
//! 第一次 Ctrl+S 才走「输入文件名 → 选择保存位置」。

use crate::ui::ime;
use crate::ui::state::{human_size, with_state, AppState, Shared};
use crate::ui::view::{self, EditorView};
use gtk::prelude::*;
use gtk::{gio, glib};
use owl_core::{
    scroll, search, Document, Hit, IndexBuilder, LineIndex, SearchOutcome, SearchQuery,
    INDEX_BATCH,
};
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

enum SearchMsg {
    Done(Vec<Hit>, SearchOutcome),
}
enum IndexMsg {
    Progress(f64),
    Done(LineIndex, u64),
}

/// 窗口层提供给标签页的外部能力。
pub struct TabEnv {
    pub window: gtk::ApplicationWindow,
    /// 窗口的动作组：切换只读/换行/状态栏时要同步菜单勾选状态。
    pub actions: gio::SimpleActionGroup,
    /// 标签页不处理的动作（新建窗口、打开、关闭标签页…）交给窗口层。
    pub window_action: Rc<dyn Fn(&str)>,
    /// 标题（文件名 / 脏标记）变化后刷新窗口标题与标签文字（内部再刷一次标签）。
    pub refresh_title: Rc<dyn Fn()>,
    /// 文件第一次落盘 / 另存成功时把路径记进「最近打开文件」。
    pub remember_path: Rc<dyn Fn(&Path)>,
}

pub struct Tab {
    pub state: Shared,
    pub view: EditorView,
    /// 标签页内容（查找栏 + 编辑区 + 状态栏）。
    pub page: gtk::Box,
    /// 标签文字（文件名，脏时带 ●）。
    pub title: gtk::Label,
    /// 统一的刷新钩子：重绘 + 状态栏 + 滚动条 + 输入法光标。
    pub hook: Rc<dyn Fn()>,
    /// 处理本文档自己的动作；返回 false 表示交给窗口层。
    pub dispatch: Rc<dyn Fn(&str) -> bool>,
    window: gtk::ApplicationWindow,
    refresh_title: Rc<dyn Fn()>,
    remember_path: Rc<dyn Fn(&Path)>,
}

impl Tab {
    /// 标签 / 窗口标题用的显示名（未命名或带 `●` 脏标记）。
    pub fn display_name(&self) -> String {
        display_name_of(&self.state)
    }

    pub fn is_dirty(&self) -> bool {
        self.state
            .borrow()
            .doc
            .as_ref()
            .map(|d| d.dirty)
            .unwrap_or(false)
    }

    /// 「干净的未命名空缓冲区」：打开文件时可以直接复用它，不用多开一个空标签。
    pub fn is_pristine(&self) -> bool {
        let s = self.state.borrow();
        s.path.is_none() && s.len() == 0 && !s.doc.as_ref().map(|d| d.dirty).unwrap_or(false)
    }

    pub fn focus(&self) {
        self.view.grab_focus();
    }

    /// 把文件装进这个已存在的标签页（打开文件时复用空白标签页，不再多开一个）。
    pub fn load_file(&self, path: PathBuf) -> Result<(), String> {
        match Document::open(&path) {
            Ok(doc) => {
                with_state(&self.state, |s| {
                    install_document(s, doc, Some(path.clone()), format!("已打开 {}", path.display()))
                });
                ime::reset(&self.state);
                (self.hook)();
                Ok(())
            }
            Err(e) => {
                let msg = format!("打开失败 {}: {}", path.display(), e);
                with_state(&self.state, |s| s.message = msg.clone());
                (self.hook)();
                Err(msg)
            }
        }
    }

    /// 保存。`force_prompt` 为 true 时等同于「另存为」。
    /// `done` 只在保存真正成功后调用（取消 / 失败不调用，调用方据此决定是否继续）。
    pub fn save(&self, force_prompt: bool, done: Rc<dyn Fn()>, abort: Rc<dyn Fn()>) {
        save_with_prompt(
            &self.state,
            &self.window,
            &self.refresh_title,
            &self.remember_path,
            force_prompt,
            done,
            abort,
        );
    }

    /// 标签页关闭时主动断开引用环并停掉后台线程。
    ///
    /// `AppState::on_change` 持有捕获了 `AppState` 的刷新钩子（`state → hook →
    /// state`），不主动断开的话，关掉的标签页连同它那份文档、行索引、
    /// 后台索引/查找线程都会跟着进程活到退出。大文件编辑一整天的场景下
    /// 这是实打实的内存泄漏。
    pub fn dispose(&self) {
        let mut s = self.state.borrow_mut();
        if let Some(c) = s.index_cancel.take() {
            c.store(true, Ordering::Relaxed);
        }
        if let Some(c) = s.search_cancel.take() {
            c.store(true, Ordering::Relaxed);
        }
        s.on_change = None;
        s.on_action = None;
        s.on_need_index = None;
        // 文档与视口缓存立即归还，不必等 Rc 引用计数自然归零。
        s.doc = None;
        s.path = None;
        s.view.clear();
        s.rows.clear();
    }
}

/// 文件名 + 脏标记，标签与状态栏共用同一口径。
pub fn display_name_of(state: &Shared) -> String {
    let s = state.borrow();
    let name = s
        .path
        .as_ref()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "未命名".to_string());
    let dirty = s.doc.as_ref().map(|d| d.dirty).unwrap_or(false);
    if dirty {
        format!("● {name}")
    } else {
        name
    }
}

/// 查找浮窗里的紧凑图标按钮：16px 图标 + 中文 tooltip。
fn find_tool_button(icon: &str, tip: &str) -> gtk::Button {
    let b = gtk::Button::from_icon_name(icon);
    if let Some(img) = b.child().and_then(|c| c.downcast::<gtk::Image>().ok()) {
        img.set_pixel_size(16);
    }
    b.set_tooltip_text(Some(tip));
    b.add_css_class("flat");
    b.add_css_class("owl-find-nav");
    b
}

pub fn build_tab(env: TabEnv, initial: Option<PathBuf>) -> Rc<Tab> {
    let state: Shared = Rc::new(RefCell::new(AppState::new()));
    let view = EditorView::new(&state);

    // ---------------------------------------------------------------- 初始文档
    // 任何标签页都持有一份可编辑文档：有路径就打开文件，没有就是未命名缓冲区。
    {
        let mut startup_msg = String::new();
        let loaded = match &initial {
            Some(p) => match Document::open(p) {
                Ok(doc) => Some((doc, Some(p.clone()), format!("已打开 {}", p.display()))),
                Err(e) => {
                    startup_msg = format!("打开失败 {}: {}", p.display(), e);
                    Document::untitled().ok().map(|doc| (doc, None, startup_msg.clone()))
                }
            },
            None => match Document::untitled() {
                Ok(doc) => Some((doc, None, "未命名缓冲区：直接输入内容，Ctrl+S 保存".to_string())),
                Err(e) => {
                    startup_msg = format!("无法创建未命名缓冲区: {e}");
                    None
                }
            },
        };
        with_state(&state, |s| {
            if let Some((doc, path, msg)) = loaded {
                install_document(s, doc, path, msg);
            } else {
                s.message = startup_msg;
            }
        });
    }

    // ---------------------------------------------------------------- 查找 / 替换浮窗
    // 旧实现是一条压在页面顶部的横栏（Revealer 插在 page 里），展开时正文
    // 整体被往下挤：行号跳位、视口行数变化、滚动位置跟着重算。改为浮在
    // 编辑区右上角的独立小卡片（GtkOverlay），不参与内容布局，
    // 出现/关闭都不改变正文的可用高度与滚动位置。
    let find_entry = gtk::SearchEntry::new();
    find_entry.set_placeholder_text(Some("查找文本；输入 #123 跳到第 123 行"));
    find_entry.set_hexpand(true);
    find_entry.set_width_chars(16);

    let find_count = gtk::Label::new(Some("—"));
    find_count.add_css_class("owl-dim");
    find_count.set_xalign(0.0);
    find_count.set_width_chars(7);

    // 「Aa」做成紧凑的开关按钮，比带文字的 CheckButton 省一行宽度。
    let case_check = gtk::ToggleButton::with_label("Aa");
    case_check.set_tooltip_text(Some("区分大小写"));
    case_check.set_active(false);
    case_check.add_css_class("flat");
    case_check.add_css_class("owl-find-toggle");

    let btn_prev = find_tool_button("go-up-symbolic", "上一个匹配（Shift+F3）");
    let btn_next = find_tool_button("go-down-symbolic", "下一个匹配（F3）");
    let btn_find_close = find_tool_button("window-close-symbolic", "关闭（Esc）");

    let head_row = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    head_row.append(&find_entry);
    head_row.append(&btn_find_close);

    let nav_spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    nav_spacer.set_hexpand(true);
    let nav_row = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    nav_row.append(&find_count);
    nav_row.append(&nav_spacer);
    nav_row.append(&case_check);
    nav_row.append(&btn_prev);
    nav_row.append(&btn_next);

    // 替换行只在 Ctrl+H 时出现：浮窗变高，但依然不挤占正文。
    let replace_entry = gtk::Entry::new();
    replace_entry.set_placeholder_text(Some("替换为"));
    replace_entry.set_hexpand(true);
    let btn_replace = gtk::Button::with_label("替换");
    let btn_replace_all = gtk::Button::with_label("全部替换");
    btn_replace.add_css_class("owl-find-btn");
    btn_replace_all.add_css_class("owl-find-btn");
    let replace_row = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    replace_row.append(&replace_entry);
    replace_row.append(&btn_replace);
    replace_row.append(&btn_replace_all);
    replace_row.set_visible(false);

    let find_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
    find_box.add_css_class("owl-findbar");
    find_box.set_width_request(396);
    find_box.append(&head_row);
    find_box.append(&nav_row);
    find_box.append(&replace_row);
    // 浮窗锚在编辑区右上角：右边留 22px 给垂直滚动条，避免把它盖住点不到。
    find_box.set_halign(gtk::Align::End);
    find_box.set_valign(gtk::Align::Start);
    find_box.set_margin_top(8);
    find_box.set_margin_end(22);
    find_box.set_visible(false);

    // Esc 关闭浮窗。用捕获阶段：焦点在 SearchEntry 里时 Esc 会被它自己
    // 拿去清空文本，冒泡阶段就收不到。
    {
        let key = gtk::EventControllerKey::new();
        key.set_propagation_phase(gtk::PropagationPhase::Capture);
        let panel = find_box.clone();
        let v = view.clone();
        key.connect_key_pressed(move |_, keyval, _, _| {
            if keyval == gtk::gdk::Key::Escape {
                panel.set_visible(false);
                v.grab_focus();
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        find_box.add_controller(key);
    }

    // ---------------------------------------------------------------- 编辑区 / 双向滚动
    // 初值必须满足 upper > page_size，否则 GTK 一开始就把滚动条置成不可交互；
    // 真正的范围由 hook() 用精确行号 / 全局最长行写入。
    let vadj = gtk::Adjustment::new(0.0, 0.0, 100.0, 1.0, 10.0, 10.0);
    let hadj = gtk::Adjustment::new(0.0, 0.0, 1000.0, 20.0, 120.0, 300.0);
    let vscroll = gtk::Scrollbar::new(gtk::Orientation::Vertical, Some(&vadj));
    let hscroll = gtk::Scrollbar::new(gtk::Orientation::Horizontal, Some(&hadj));
    // 默认自动换行：一开始就没有横向溢出，先把横向滚动条藏起来，
    // 之后每次 hook 会按 word_wrap 决定显隐。
    hscroll.set_visible(false);
    // 独立 GtkScrollbar 在 GtkGrid 里可能只拿到 0 尺寸（看得见却点不到）。
    // 改用 Box 布局 + 显式尺寸请求，保证滑块有可拖动的实体面积。
    vscroll.set_width_request(14);
    vscroll.set_vexpand(true);
    hscroll.set_height_request(14);
    hscroll.set_hexpand(true);

    let content_row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    content_row.set_hexpand(true);
    content_row.set_vexpand(true);
    content_row.append(&view);
    content_row.append(&vscroll);

    let content_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content_box.set_hexpand(true);
    content_box.set_vexpand(true);
    content_box.append(&content_row);
    content_box.append(&hscroll);

    // ---------------------------------------------------------------- 状态栏
    let status = gtk::Label::new(Some("就绪"));
    status.set_xalign(0.0);
    status.set_ellipsize(gtk::pango::EllipsizeMode::End);
    status.add_css_class("monospace");
    status.add_css_class("owl-status");
    status.set_margin_start(10);
    status.set_margin_end(10);
    status.set_margin_top(4);
    status.set_margin_bottom(4);

    // 编辑区 + 浮层：查找框作为 overlay 子件叠在编辑区之上，
    // 而不是作为兄弟件插进纵向布局去挤压正文。
    let editor_layer = gtk::Overlay::new();
    editor_layer.set_hexpand(true);
    editor_layer.set_vexpand(true);
    editor_layer.set_child(Some(&content_box));
    editor_layer.add_overlay(&find_box);

    let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
    page.append(&editor_layer);
    page.append(&status);

    let title = gtk::Label::new(Some(&display_name_of(&state)));
    title.set_xalign(0.0);

    state.borrow_mut().vadj = Some(vadj.clone());
    state.borrow_mut().hadj = Some(hadj.clone());
    state.borrow_mut().hscroll = Some(hscroll.clone());

    // ---------------------------------------------------------------- 刷新钩子
    let syncing = Rc::new(Cell::new(false));
    let hook: Rc<dyn Fn()> = {
        let state = state.clone();
        let view = view.clone();
        let status = status.clone();
        let vadj = vadj.clone();
        let hadj = hadj.clone();
        let hscroll = hscroll.clone();
        let find_count = find_count.clone();
        let syncing = syncing.clone();
        let actions = env.actions.clone();
        let title = title.clone();
        let refresh_title = env.refresh_title.clone();
        Rc::new(move || {
            // 先解析视口，保证 longest-line / 行数是最新的。
            // 索引构建完成时总行数会从估算变成精确值，这里把状态也钳到与
            // adjustment 完全相同的口径，避免 top_line / x_offset 越界导致
            // 视图与滚动条错位、内容突然跳一下。
            {
                let mut s = state.borrow_mut();
                let max_top = s.scroll_max_top();
                if s.top_line > max_top {
                    s.top_line = max_top;
                    s.view_dirty = true;
                }
                s.refresh_view();
                let (_, _, hv) = s.hscroll_params();
                if (s.x_offset - hv).abs() > 0.5 {
                    s.x_offset = hv;
                }
            }
            let (vu, vp, vv) = state.borrow().scroll_params();
            let (hu, hp, hv) = state.borrow().hscroll_params();
            // 关键：只在值确实不一致时写。用户拖动滚动条时 value_changed 已把
            // top_line / x_offset 同步成控件当前值，因此这里一个 set_value 都不会
            // 执行，GTK 的拖拽手势不会被中途回写打断。
            syncing.set(true);
            if scroll::needs_write(vadj.upper(), vu, 0.5) { vadj.set_upper(vu); }
            if scroll::needs_write(vadj.page_size(), vp, 0.5) { vadj.set_page_size(vp); }
            if scroll::needs_write(vadj.value(), vv, 0.5) { vadj.set_value(vv); }
            if scroll::needs_write(hadj.upper(), hu, 1.0) { hadj.set_upper(hu); }
            if scroll::needs_write(hadj.page_size(), hp, 1.0) { hadj.set_page_size(hp); }
            if scroll::needs_write(hadj.value(), hv, 1.0) { hadj.set_value(hv); }
            // 自动换行时没有横向溢出，横向滚动条隐藏；关闭换行后才需要它。
            hscroll.set_visible(!state.borrow().word_wrap);
            syncing.set(false);

            let status_visible = status.is_visible();
            let (text, hits, idx, flags) = {
                let mut s = state.borrow_mut();
                let t = s.status_text();
                (
                    t,
                    s.hits.len(),
                    s.hit_index,
                    (s.read_only, s.show_line_numbers, s.word_wrap, status_visible),
                )
            };
            status.set_label(&text);
            find_count.set_label(&if hits == 0 {
                "—".to_string()
            } else {
                format!("{}/{}", idx + 1, hits)
            });
            sync_action_states(&actions, flags.0, flags.1, flags.2, flags.3);
            // 文件名可能刚变（另存为 / 首次保存 / 变脏），标签与窗口标题同步刷新。
            title.set_text(&display_name_of(&state));
            refresh_title();
            ime::update_cursor_location(&state, &view);
            view.queue_draw();

            // 索引被编辑作废时，自动在后台重建。
            let (need, starter) = {
                let s = state.borrow();
                (
                    s.has_doc() && s.line_index.is_none() && !s.index_building,
                    s.on_need_index.clone(),
                )
            };
            if need {
                if let Some(f) = starter {
                    f();
                }
            }
        })
    };
    state.borrow_mut().on_change = Some(hook.clone());

    // 滚动条拖动时的轻量刷新：重建视口 + 重绘 + 状态栏，
    // **完全不碰 adjustment**，否则会形成「用户拖动 → 回写 → 拖拽被打断」的环。
    let light: Rc<dyn Fn()> = {
        let state = state.clone();
        let view = view.clone();
        let status = status.clone();
        Rc::new(move || {
            state.borrow_mut().refresh_view();
            let text = state.borrow_mut().status_text();
            status.set_label(&text);
            view.queue_draw();
        })
    };

    // 垂直滚动条：value 即精确行号。
    {
        let state = state.clone();
        let light = light.clone();
        let syncing = syncing.clone();
        vadj.connect_value_changed(move |a| {
            if syncing.get() {
                return;
            }
            let value = a.value().max(0.0);
            let changed = {
                let mut s = state.borrow_mut();
                // 自动换行下 value 的整数部分是逻辑行号，小数部分编码行内
                // 显示行下标；不换行时恒为整数。
                let (line, sub) = if s.word_wrap {
                    owl_core::scroll::top_from_value(value, s.first_line_rows)
                } else {
                    (value as u64, 0usize)
                };
                let line = line.min(s.scroll_max_top());
                if s.top_line == line && s.top_sub == sub {
                    false
                } else {
                    s.top_line = line;
                    s.top_sub = sub;
                    s.view_dirty = true;
                    true
                }
            };
            if changed {
                light();
            }
        });
    }
    // 水平滚动条：value 即像素偏移。
    {
        let state = state.clone();
        let light = light.clone();
        let syncing = syncing.clone();
        hadj.connect_value_changed(move |a| {
            if syncing.get() {
                return;
            }
            let x = a.value().max(0.0);
            let changed = {
                let mut s = state.borrow_mut();
                if (s.x_offset - x).abs() < 0.5 {
                    false
                } else {
                    s.x_offset = x;
                    true
                }
            };
            if changed {
                light();
            }
        });
    }

    // ---------------------------------------------------------------- 动作分发（文档级）
    let dispatch: Rc<dyn Fn(&str) -> bool> = {
        let state = state.clone();
        let view = view.clone();
        let window = env.window.clone();
        let hook = hook.clone();
        let refresh_title = env.refresh_title.clone();
        let remember_path = env.remember_path.clone();
        let find_box_for_actions = find_box.clone();
        let find_entry = find_entry.clone();
        let replace_row = replace_row.clone();
        let status = status.clone();

        Rc::new(move |name: &str| -> bool {
            // ---- 窗口级动作：交给窗口层（新建标签页 / 新窗口 / 打开 / 关闭…）
            if matches!(
                name,
                "new"
                    | "new_window"
                    | "open"
                    | "close"
                    | "quit"
                    | "next_tab"
                    | "prev_tab"
                    | "theme_light"
                    | "theme_dark"
                    | "fullscreen"
            ) {
                return false;
            }
            match name {
                "reload" => {
                    let (path, dirty) = {
                        let s = state.borrow();
                        (
                            s.path.clone(),
                            s.doc.as_ref().map(|d| d.dirty).unwrap_or(false),
                        )
                    };
                    let Some(p) = path else {
                        with_state(&state, |s| {
                            s.message = "未命名缓冲区没有可重新载入的文件".to_string()
                        });
                        hook();
                        return true;
                    };
                    if dirty {
                        with_state(&state, |s| {
                            s.message = "文件已修改，请先保存或撤销后再重新载入".to_string()
                        });
                        hook();
                        return true;
                    }
                    match Document::open(&p) {
                        Ok(doc) => {
                            with_state(&state, |s| {
                                install_document(s, doc, Some(p.clone()), format!("已重新载入 {}", p.display()))
                            });
                            ime::reset(&state);
                        }
                        Err(e) => with_state(&state, |s| {
                            s.message = format!("重新载入失败 {}: {}", p.display(), e)
                        }),
                    }
                    hook();
                }
                "save" => save_with_prompt(
                    &state,
                    &window,
                    &refresh_title,
                    &remember_path,
                    false,
                    Rc::new(|| {}),
                    Rc::new(|| {}),
                ),
                "save_as" => save_with_prompt(
                    &state,
                    &window,
                    &refresh_title,
                    &remember_path,
                    true,
                    Rc::new(|| {}),
                    Rc::new(|| {}),
                ),
                "undo" | "redo" => {
                    // 走状态层的增量索引维护，不再丢弃 line_index 触发全文件
                    // 后台重建（否则总行数从精确值跌回估算值、视口被拉错、
                    // 索引 tick 连续重绘，快捷键撤销时表现为闪烁）。
                    with_state(&state, |s| s.undo_or_redo(name == "undo"));
                }
                "cut" | "copy" => {
                    let text = { state.borrow_mut().selected_text() };
                    if !text.is_empty() {
                        view.clipboard().set_text(&text);
                        if name == "cut" {
                            with_state(&state, |s| {
                                let (a, b) = s.selection_range().unwrap_or((s.cursor, s.cursor));
                                s.delete_range(a, b);
                            });
                        }
                    }
                }
                "paste" => {
                    let cb = view.clipboard();
                    let s = state.clone();
                    let v = view.clone();
                    cb.read_text_async(None::<&gio::Cancellable>, move |res| {
                        if let Ok(Some(text)) = res {
                            with_state(&s, |st| st.insert_text(text.as_str()));
                            ime::update_cursor_location(&s, &v);
                        }
                    });
                }
                "select_all" => with_state(&state, |s| s.select_all()),
                "delete_line" | "duplicate_line" => {
                    with_state(&state, |s| {
                        let Some((ls, le)) = s.row_bounds() else { return; };
                        let len = s.len();
                        let line_end = (le + 1).min(len);
                        if name == "delete_line" {
                            s.cursor = ls;
                            s.sel_anchor = None;
                            s.delete_range(ls, line_end);
                        } else {
                            let mut data = vec![0u8; (line_end - ls) as usize];
                            let n = s.doc.as_mut().map(|d| d.read_at(ls, &mut data)).unwrap_or(0);
                            data.truncate(n);
                            if !data.ends_with(b"\n") {
                                data.push(b'\n');
                            }
                            let text = String::from_utf8_lossy(&data).into_owned();
                            s.cursor = ls;
                            s.sel_anchor = None;
                            s.insert_text(&text);
                        }
                    });
                }
                "toggle_readonly" => with_state(&state, |s| {
                    s.read_only = !s.read_only;
                    s.message = if s.read_only {
                        "已开启只读模式".into()
                    } else {
                        "已关闭只读模式，可直接编辑".into()
                    };
                }),
                "toggle_line_numbers" => with_state(&state, |s| {
                    s.show_line_numbers = !s.show_line_numbers;
                    s.view_dirty = true;
                }),
                "toggle_wrap" => with_state(&state, |s| {
                    s.set_word_wrap(!s.word_wrap);
                }),
                "toggle_status" => {
                    let vis = status.is_visible();
                    status.set_visible(!vis);
                    hook();
                }
                "zoom_in" | "zoom_out" | "zoom_reset" => {
                    with_state(&state, |s| {
                        s.font_size = match name {
                            "zoom_in" => (s.font_size + 1).clamp(8, 36),
                            "zoom_out" => (s.font_size - 1).clamp(8, 36),
                            _ => 12,
                        };
                        s.font.set_size(s.font_size * gtk::pango::SCALE);
                        // 字宽变了，按旧字宽累计的最长行像素值作废。
                        s.doc_max_line_px = 0.0;
                        s.max_line_px = 0.0;
                        // 字宽变了折行数也变了，滚动条长度重新学习。
                        s.reset_wrap_learning();
                    });
                    view::initial_metrics(&view, &state);
                    hook();
                }
                "find" | "replace" => {
                    find_box_for_actions.set_visible(true);
                    replace_row.set_visible(name == "replace");
                    find_entry.grab_focus();
                    find_entry.select_region(0, -1);
                }
                "goto" => {
                    find_box_for_actions.set_visible(true);
                    replace_row.set_visible(false);
                    find_entry.set_text("#");
                    find_entry.grab_focus();
                    find_entry.set_position(-1);
                }
                "find_next" | "find_prev" => {
                    let dir = if name == "find_next" { 1 } else { -1 };
                    with_state(&state, |s| {
                        if s.hits.is_empty() {
                            s.message = "请先查找文本".to_string();
                            return;
                        }
                        let n = s.hits.len() as i32;
                        s.hit_index = ((s.hit_index as i32 + dir).rem_euclid(n)) as usize;
                        let h = s.hits[s.hit_index];
                        s.jump_to(h.start);
                        s.sel_anchor = Some(h.start);
                        s.cursor = h.end;
                        s.view_dirty = true;
                    });
                    hook();
                    view.grab_focus();
                }
                "doc_start" => with_state(&state, |s| s.jump_to_line(0)),
                "doc_end" => {
                    let total = { state.borrow().total_lines() };
                    with_state(&state, |s| s.jump_to_line(total.saturating_sub(1)));
                }
                "word_count" => {
                    let (chars, words, lines, truncated) = {
                        let mut s = state.borrow_mut();
                        match s.doc.as_mut() {
                            Some(d) => {
                                let n = d.len().min(64 * 1024 * 1024) as usize;
                                let data = d.read_range(0, n);
                                let text = String::from_utf8_lossy(&data);
                                (
                                    text.chars().count(),
                                    text.split_whitespace().count(),
                                    text.matches('\n').count() + 1,
                                    d.len() > n as u64,
                                )
                            }
                            None => (0, 0, 0, false),
                        }
                    };
                    let note = if truncated {
                        "\n\n（文件超过 64 MiB，仅统计前 64 MiB）"
                    } else {
                        ""
                    };
                    info_dialog(
                        &window,
                        "字数统计",
                        &format!("字符数：{chars}\n词数：{words}\n行数：{lines}{note}"),
                    );
                }
                "line_ending_lf" | "line_ending_crlf" => {
                    let to_crlf = name == "line_ending_crlf";
                    with_state(&state, |s| {
                        let len = s.len();
                        if len > 64 * 1024 * 1024 {
                            s.message = "文件超过 64 MiB，暂不支持整篇行尾转换".to_string();
                            return;
                        }
                        let data = s
                            .doc
                            .as_mut()
                            .map(|d| d.read_range(0, len as usize))
                            .unwrap_or_default();
                        let mut out = Vec::with_capacity(data.len());
                        let mut i = 0;
                        while i < data.len() {
                            if data[i] == b'\r' {
                                i += 1;
                                continue;
                            }
                            if data[i] == b'\n' {
                                if to_crlf {
                                    out.push(b'\r');
                                }
                                out.push(b'\n');
                            } else {
                                out.push(data[i]);
                            }
                            i += 1;
                        }
                        if let Some(d) = s.doc.as_mut() {
                            d.replace(0, len, &out);
                        }
                        s.line_index = None;
                        s.revision += 1;
                        s.line_ending = if to_crlf { "CRLF" } else { "LF" };
                        s.view_dirty = true;
                        s.message = format!("已转换为 {}", s.line_ending);
                    });
                }
                "print" => do_print(&window, &state),
                "shortcuts" => info_dialog(
                    &window,
                    "快捷键一览",
                    "文件：Ctrl+N 新建标签页 · Ctrl+Shift+N 新建窗口 · Ctrl+O 打开 · Ctrl+S 保存 · Ctrl+Shift+S 另存为 · F5 重新载入 · Ctrl+P 打印\n\
                     标签页：Ctrl+W 关闭当前标签页 · Ctrl+Tab / Ctrl+PageDown 下一个 · Ctrl+Shift+Tab 上一个\n\
                     编辑：Ctrl+Z 撤销 · Ctrl+Y 重做 · Ctrl+C/V/X 复制粘贴剪切 · Ctrl+A 全选 · Ctrl+D 删除行 · Tab 缩进\n\
                     查找：Ctrl+F 查找 · Ctrl+H 替换（Ctrl+R 亦可） · F3 / Shift+F3 下一个/上一个 · Ctrl+G 转到行 · 查找框输入 #123 跳行\n\
                     视图：Ctrl++ / Ctrl+- / Ctrl+0 缩放 · F11 全屏\n\
                     导航：方向键 · Home / End · PageUp / PageDown · Ctrl+Home / Ctrl+End 文件首尾\n\
                     中文输入：ibus-rime 直接输入，Wayland text-input-v3，无需环境变量",
                ),
                "about" => info_dialog(
                    &window,
                    "关于 OwlEditor",
                    "OwlEditor 0.1.0\n\
                     Linux 原生大文件文本编辑器 · Rust + GTK4\n\n\
                     对标 Kate / EmEditor / Notepad-- 的菜单与基础功能；\n\
                     不依赖 KDE 运行环境，打开 10 GiB 文本首屏立即可用，\n\
                     后台渐进建立精确行索引，内存预算 200 MiB。\n\n\
                     一窗多标签 · 多窗口 · 未命名缓冲区可直接输入\n\
                     UTF-8 优先 · Wayland text-input-v3 原生中文输入",
                ),
                _ => {}
            }
            true
        })
    };
    state.borrow_mut().on_action = Some({
        let dispatch = dispatch.clone();
        let window_action = env.window_action.clone();
        Rc::new(move |name: &str| {
            if (dispatch)(name) {
                return;
            }
            (window_action)(name);
        })
    });

    // ---------------------------------------------------------------- 查找 / 替换交互
    {
        let state = state.clone();
        let hook = hook.clone();
        let case_check = case_check.clone();
        let pending: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));
        find_entry.connect_search_changed(move |e| {
            let text = e.text().to_string();
            if let Some(id) = pending.borrow_mut().take() {
                id.remove();
            }
            let s = state.clone();
            let h = hook.clone();
            let cs = case_check.is_active();
            let id = glib::timeout_add_local_once(Duration::from_millis(220), move || {
                if let Some(line_no) = text.strip_prefix('#') {
                    if let Ok(n) = line_no.trim().parse::<u64>() {
                        with_state(&s, |st| {
                            st.jump_to_line(n.saturating_sub(1));
                            st.hits.clear();
                            st.search_msg = format!("已跳到第 {} 行", n);
                            if !st.index_exact() {
                                st.search_msg.push_str("（行号精修中）");
                            }
                        });
                        h();
                        return;
                    }
                }
                start_search(&s, &h, text, cs);
            });
            *pending.borrow_mut() = Some(id);
        });
    }
    {
        let s = state.clone();
        replace_entry.connect_changed(move |e| {
            s.borrow_mut().replace_text = e.text().to_string();
        });
    }
    {
        let s = state.clone();
        let h = hook.clone();
        let cc = case_check.clone();
        let cc_in = cc.clone();
        cc.connect_toggled(move |_| {
            let q = { s.borrow().search_query.clone() };
            if !q.is_empty() {
                start_search(&s, &h, q, cc_in.is_active());
            }
        });
    }
    {
        let d = dispatch.clone();
        find_entry.connect_activate(move |_| {
            (d)("find_next");
        });
        let d = dispatch.clone();
        btn_next.connect_clicked(move |_| {
            (d)("find_next");
        });
        let d = dispatch.clone();
        btn_prev.connect_clicked(move |_| {
            (d)("find_prev");
        });
    }
    // 替换一处：按钮和「替换为」输入框回车共用同一段逻辑。
    {
        let state = state.clone();
        let hook = hook.clone();
        let entry_in = replace_entry.clone();
        let do_replace_one: Rc<dyn Fn()> = Rc::new(move || {
            let text = entry_in.text().to_string();
            with_state(&state, |s| {
                if s.hits.is_empty() {
                    s.message = "没有可替换的匹配".to_string();
                    return;
                }
                let h = s.hits[s.hit_index.min(s.hits.len() - 1)];
                s.cursor = h.end;
                s.sel_anchor = Some(h.start);
                s.insert_text(&text);
                s.hits.clear();
                s.hit_index = 0;
                s.message = "已替换 1 处".to_string();
            });
            hook();
        });
        let r = do_replace_one.clone();
        btn_replace.connect_clicked(move |_| (*r)());
        replace_entry.connect_activate(move |_| (*do_replace_one)());
    }
    {
        let state = state.clone();
        let replace_entry = replace_entry.clone();
        btn_replace_all.connect_clicked(move |_| {
            let text = replace_entry.text().to_string();
            with_state(&state, |s| {
                let hits = s.hits.clone();
                if hits.is_empty() {
                    s.message = "没有可替换的匹配".to_string();
                    return;
                }
                if let Some(d) = s.doc.as_mut() {
                    // 从后往前替换，前面的偏移不受影响。
                    for h in hits.iter().rev() {
                        d.replace(h.start, h.end, text.as_bytes());
                    }
                }
                s.line_index = None;
                s.revision += 1;
                s.view_dirty = true;
                s.hits.clear();
                s.hit_index = 0;
                s.message = format!("已替换 {} 处（后台重建行索引中）", hits.len());
            });
        });
    }
    {
        // 关闭浮窗后把焦点交还编辑区，否则键盘输入会停在已隐藏的输入框上。
        let panel = find_box.clone();
        let v = view.clone();
        btn_find_close.connect_clicked(move |_| {
            panel.set_visible(false);
            v.grab_focus();
        });
    }

    // ---------------------------------------------------------------- 后台索引触发
    state.borrow_mut().on_need_index = Some({
        let s = state.clone();
        let h = hook.clone();
        let st_label = status.clone();
        Rc::new(move || start_index(&s, &h, st_label.clone()))
    });

    view::initial_metrics(&view, &state);
    hook();

    Rc::new(Tab {
        state,
        view,
        page,
        title,
        hook,
        dispatch,
        window: env.window,
        refresh_title: env.refresh_title,
        remember_path: env.remember_path,
    })
}

/// 把一个已打开的文档装进状态，并重置所有视口相关量。
fn install_document(s: &mut AppState, mut doc: Document, path: Option<PathBuf>, msg: String) {
    // 探一次行尾样式（只读文件头 64 KiB）。
    let mut probe = vec![0u8; 64 * 1024];
    let n = doc.read_at(0, &mut probe);
    let le = detect_line_ending(&probe[..n]);
    s.doc = Some(doc);
    s.path = path;
    s.top_line = 0;
    s.top_sub = 0;
    s.first_line_rows = 1;
    s.wrap_ratio = 1.0;
    s.cursor = 0;
    s.sel_anchor = None;
    s.x_offset = 0.0;
    s.goal_col = 0;
    s.max_line_px = 0.0;
    s.doc_max_line_px = 0.0;
    s.line_index = None;
    s.index_progress = 0.0;
    s.index_building = false;
    s.revision += 1;
    s.hits.clear();
    s.hit_index = 0;
    s.search_msg.clear();
    s.read_only = false;
    s.line_ending = le;
    s.view_dirty = true;
    s.message = msg;
}

fn detect_line_ending(data: &[u8]) -> &'static str {
    let crlf = data.windows(2).filter(|w| w == b"\r\n").count();
    let lf = data.iter().filter(|&&b| b == b'\n').count();
    if crlf > 0 && crlf * 2 >= lf {
        "CRLF"
    } else {
        "LF"
    }
}

fn info_dialog(window: &gtk::ApplicationWindow, title: &str, body: &str) {
    let d = gtk::Window::builder()
        .transient_for(window)
        .modal(true)
        .title(title)
        .default_width(460)
        .build();
    d.add_css_class("owleditor");
    let vb = gtk::Box::new(gtk::Orientation::Vertical, 12);
    vb.set_margin_start(20);
    vb.set_margin_end(20);
    vb.set_margin_top(20);
    vb.set_margin_bottom(20);
    let label = gtk::Label::new(Some(body));
    label.set_xalign(0.0);
    label.set_wrap(true);
    label.set_selectable(true);
    vb.append(&label);
    let close = gtk::Button::with_label("关闭");
    close.set_halign(gtk::Align::End);
    let dd = d.clone();
    close.connect_clicked(move |_| dd.close());
    vb.append(&close);
    d.set_child(Some(&vb));
    d.present();
}

// ---------------------------------------------------------------- 保存

/// 保存入口。「未命名缓冲区」或「另存为」时：
/// ① 先弹一个输入文件名的对话框（预填当前名 / 未命名.txt）；
/// ② 确认后打开系统保存对话框选目录（文件名已带过去）；
/// ③ 写盘成功后更新标题、最近打开文件，最后才调用 `done`。
pub fn save_with_prompt(
    state: &Shared,
    window: &gtk::ApplicationWindow,
    refresh_title: &Rc<dyn Fn()>,
    remember_path: &Rc<dyn Fn(&Path)>,
    force_prompt: bool,
    done: Rc<dyn Fn()>,
    abort: Rc<dyn Fn()>,
) {
    let (untitled, current_name) = {
        let s = state.borrow();
        let name = s
            .path
            .as_ref()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()));
        (s.path.is_none(), name)
    };

    // 有路径且不是「另存为」：直接写回。
    if !force_prompt && !untitled {
        match save_document(state) {
            Ok((n, p)) => {
                with_state(state, |s| {
                    s.message = format!("已保存 {} ({})", p.display(), human_size(n))
                });
                refresh_title();
                done();
            }
            Err(e) => {
                with_state(state, |s| s.message = format!("保存失败: {e}"));
                abort();
            }
        }
        return;
    }

    let suggested = current_name.unwrap_or_else(|| "未命名.txt".to_string());
    let window = window.clone();
    let parent = window.clone();
    let state = state.clone();
    let refresh_title = refresh_title.clone();
    let remember_path = remember_path.clone();
    let abort_cancel = abort.clone();
    ask_file_name(&parent, &suggested, move || abort_cancel(), move |name| {
        let dlg = gtk::FileDialog::builder()
            .title("选择保存位置")
            .modal(true)
            .initial_name(name.clone())
            .build();
        let s = state.clone();
        let rt = refresh_title.clone();
        let rp = remember_path.clone();
        let d2 = done.clone();
        let abort_save = abort.clone();
        let abort_choose = abort.clone();
        dlg.save(Some(&window), None::<&gio::Cancellable>, move |res| {
            let Ok(file) = res else {
                // 用户在系统保存对话框里取消：交给调用方复位（关闭窗口的流程要允许重试）。
                abort_choose();
                return;
            };
            let Some(path) = file.path() else {
                abort_choose();
                return;
            };
            let r = {
                let mut st = s.borrow_mut();
                match st.doc.as_mut() {
                    Some(d) => d.save_to(&path).map(|n| (n, path.clone())),
                    None => Err(std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        "没有可保存的文档",
                    )),
                }
            };
            match r {
                Ok((n, p)) => {
                    with_state(&s, |st| {
                        st.path = Some(p.clone());
                        st.message = format!("已保存 {} ({})", p.display(), human_size(n));
                    });
                    // 首次落盘 / 另存为：记进「最近打开文件」。
                    rp(&p);
                    rt();
                    d2();
                }
                Err(e) => {
                    with_state(&s, |st| st.message = format!("保存失败: {e}"));
                    abort_save();
                }
            }
        });
    });
}

/// 「请输入文件名」对话框：确认后才进入系统保存对话框选目录。
fn ask_file_name(
    parent: &gtk::ApplicationWindow,
    suggested: &str,
    on_cancel: impl Fn() + 'static,
    on_ok: impl Fn(String) + 'static,
) {
    let d = gtk::Window::builder()
        .transient_for(parent)
        .modal(true)
        .title("保存文件")
        .default_width(480)
        .build();
    d.add_css_class("owleditor");

    let vb = gtk::Box::new(gtk::Orientation::Vertical, 10);
    vb.set_margin_start(20);
    vb.set_margin_end(20);
    vb.set_margin_top(18);
    vb.set_margin_bottom(18);

    let label = gtk::Label::new(Some("请输入文件名称："));
    label.set_xalign(0.0);
    let entry = gtk::Entry::new();
    entry.set_text(suggested);
    entry.select_region(0, -1);
    entry.set_activates_default(true);
    let hint = gtk::Label::new(Some("确认后再选择保存到哪个文件夹。"));
    hint.set_xalign(0.0);
    hint.add_css_class("owl-dim");

    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.set_halign(gtk::Align::End);
    let cancel = gtk::Button::with_label("取消");
    let ok = gtk::Button::with_label("选择位置并保存");
    ok.add_css_class("suggested-action");
    row.append(&cancel);
    row.append(&ok);

    vb.append(&label);
    vb.append(&entry);
    vb.append(&hint);
    vb.append(&row);
    d.set_child(Some(&vb));

    let cb: Rc<RefCell<Option<Box<dyn Fn(String)>>>> = Rc::new(RefCell::new(Some(Box::new(on_ok))));
    let fire: Rc<dyn Fn()> = {
        let d = d.clone();
        let entry = entry.clone();
        let cb = cb.clone();
        Rc::new(move || {
            let name = entry.text().to_string().trim().to_string();
            if name.is_empty() {
                return;
            }
            d.close();
            if let Some(f) = cb.borrow_mut().take() {
                f(name);
            }
        })
    };
    {
        let fire = fire.clone();
        ok.connect_clicked(move |_| fire());
    }
    {
        let fire = fire.clone();
        entry.connect_activate(move |_| fire());
    }
    {
        let d = d.clone();
        let on_cancel = Rc::new(RefCell::new(Some(Box::new(on_cancel))));
        cancel.connect_clicked(move |_| {
            d.close();
            if let Some(f) = on_cancel.borrow_mut().take() {
                f();
            }
        });
    }

    d.present();
    entry.grab_focus();
}

fn save_document(state: &Shared) -> Result<(u64, PathBuf), String> {
    let result = {
        let mut s = state.borrow_mut();
        match s.doc.as_mut() {
            Some(d) => d.save().map(|n| (n, d.path.clone())),
            None => Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "尚未打开文件",
            )),
        }
    };
    result.map_err(|e| e.to_string())
}

// ---------------------------------------------------------------- 后台任务

/// 后台流式建立全文件行索引；UI 全程不阻塞，进度显示在状态栏。
fn start_index(state: &Shared, hook: &Rc<dyn Fn()>, status: gtk::Label) {
    let (mut builder, revision) = {
        let mut s = state.borrow_mut();
        if s.index_building || !s.has_doc() {
            return;
        }
        let Some(doc) = s.doc.as_ref() else { return; };
        let Ok(snap) = doc.snapshot() else { return; };
        s.index_building = true;
        s.index_progress = 0.0;
        (IndexBuilder::new(snap), s.revision)
    };
    let cancel = Arc::new(AtomicBool::new(false));
    if let Some(old) = state.borrow_mut().index_cancel.take() {
        old.store(true, Ordering::Relaxed);
    }
    state.borrow_mut().index_cancel = Some(cancel.clone());
    let (tx, rx) = mpsc::channel::<IndexMsg>();
    std::thread::spawn(move || {
        loop {
            if cancel.load(Ordering::Relaxed) {
                return;
            }
            let done = builder.step(INDEX_BATCH);
            let _ = tx.send(IndexMsg::Progress(builder.progress()));
            if done {
                let _ = tx.send(IndexMsg::Done(builder.finish(), revision));
                break;
            }
        }
    });
    let state = state.clone();
    let hook = hook.clone();
    glib::timeout_add_local(Duration::from_millis(40), move || {
        let mut done_msg = None;
        loop {
            match rx.try_recv() {
                Ok(IndexMsg::Progress(p)) => state.borrow_mut().index_progress = p,
                Ok(IndexMsg::Done(idx, rev)) => {
                    done_msg = Some((idx, rev));
                    break;
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    state.borrow_mut().index_building = false;
                    return glib::ControlFlow::Break;
                }
            }
        }
        if let Some((idx, rev)) = done_msg {
            let restart = {
                let mut s = state.borrow_mut();
                s.index_building = false;
                if rev == s.revision {
                    s.line_index = Some(idx);
                    s.index_progress = 1.0;
                    s.view_dirty = true;
                    s.message = format!("行索引就绪（{} 行）", s.total_lines());
                    false
                } else {
                    // 构建期间文档被编辑，结果作废，稍后自动重启。
                    s.index_progress = 0.0;
                    true
                }
            };
            hook();
            if restart {
                let f = { state.borrow().on_need_index.clone() };
                if let Some(f) = f {
                    glib::idle_add_local_once(move || f());
                }
            }
            return glib::ControlFlow::Break;
        }
        // 进度只更新状态栏文字。
        // 绝不调用 hook()/on_change：索引构建在大文件上会持续数秒，40ms 一次的
        // adjustment 回写会不断打断用户正在进行的滚动条拖拽手势。
        let text = state.borrow_mut().status_text();
        status.set_label(&text);
        glib::ControlFlow::Continue
    });
}

/// 后台流式查找：结果增量回传，随时可取消，内存恒定。
fn start_search(state: &Shared, hook: &Rc<dyn Fn()>, query: String, case_sensitive: bool) {
    {
        let mut s = state.borrow_mut();
        if let Some(c) = s.search_cancel.take() {
            c.store(true, Ordering::Relaxed);
        }
        s.search_query = query.clone();
        s.case_sensitive = case_sensitive;
        s.hits.clear();
        s.hit_index = 0;
        if query.is_empty() {
            s.search_msg.clear();
        }
    }
    if query.is_empty() {
        hook();
        return;
    }
    let snapshot = {
        state
            .borrow()
            .doc
            .as_ref()
            .and_then(|d| d.snapshot().ok())
    };
    let Some(mut snapshot) = snapshot else { return; };
    let cancel = Arc::new(AtomicBool::new(false));
    state.borrow_mut().search_cancel = Some(cancel.clone());
    let (tx, rx) = mpsc::channel::<SearchMsg>();
    std::thread::spawn(move || {
        let q = SearchQuery::new(query.as_bytes().to_vec()).case_insensitive(!case_sensitive);
        let mut hits = Vec::new();
        let outcome = search::search(&mut snapshot, &q, 0, 500, &cancel, |h| hits.push(h));
        let _ = tx.send(SearchMsg::Done(hits, outcome));
    });
    let state = state.clone();
    let hook = hook.clone();
    glib::timeout_add_local(Duration::from_millis(16), move || {
        match rx.try_recv() {
            Ok(SearchMsg::Done(hits, outcome)) => {
                with_state(&state, |s| {
                    s.hits = hits;
                    s.hit_index = 0;
                    let base = if outcome.cancelled {
                        "查找已取消".to_string()
                    } else {
                        format!("找到 {} 处（已扫描 {}）", s.hits.len(), human_size(outcome.scanned))
                    };
                    s.search_msg = base.clone();
                    s.message = base;
                    s.view_dirty = true;
                });
                hook();
                glib::ControlFlow::Break
            }
            Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
            Err(mpsc::TryRecvError::Disconnected) => glib::ControlFlow::Break,
        }
    });
}

fn do_print(window: &gtk::ApplicationWindow, state: &Shared) {
    let lines: Vec<String> = {
        let mut s = state.borrow_mut();
        match s.doc.as_mut() {
            Some(d) => {
                let n = d.len().min(1024 * 1024) as usize;
                let data = d.read_range(0, n);
                let (ranges, _) = owl_core::utf8::split_lines(&data);
                ranges
                    .iter()
                    .map(|&(a, b)| String::from_utf8_lossy(&data[a..b]).into_owned())
                    .collect()
            }
            None => Vec::new(),
        }
    };
    const PER_PAGE: usize = 55;
    let pages = (lines.len() / PER_PAGE + 1).max(1) as i32;
    let op = gtk::PrintOperation::new();
    op.set_job_name("OwlEditor");
    op.set_show_progress(true);
    let p = pages;
    op.connect_begin_print(move |o, _| o.set_n_pages(p));
    let content = Rc::new(lines);
    let c2 = content.clone();
    op.connect_draw_page(move |_, ctx, page| {
        let cr = ctx.cairo_context();
        let _ = cr.set_source_rgb(0.08, 0.09, 0.11);
        cr.select_font_face(
            "Monospace",
            gtk::cairo::FontSlant::Normal,
            gtk::cairo::FontWeight::Normal,
        );
        cr.set_font_size(9.5);
        let start = page as usize * PER_PAGE;
        for (i, line) in c2.iter().skip(start).take(PER_PAGE).enumerate() {
            cr.move_to(36.0, 36.0 + (i as f64 + 1.0) * 12.0);
            let _ = cr.show_text(line);
        }
    });
    let _ = op.run(gtk::PrintOperationAction::PrintDialog, Some(window));
}

fn sync_action_states(
    group: &gio::SimpleActionGroup,
    readonly: bool,
    numbers: bool,
    wrap: bool,
    status: bool,
) {
    for (name, value) in [
        ("toggle_readonly", readonly),
        ("toggle_line_numbers", numbers),
        ("toggle_wrap", wrap),
        ("toggle_status", status),
    ] {
        if let Some(a) = group.lookup_action(name) {
            if let Ok(simple) = a.downcast::<gio::SimpleAction>() {
                let cur = simple
                    .state()
                    .and_then(|v| v.get::<bool>())
                    .unwrap_or(false);
                if cur != value {
                    simple.set_state(&value.to_variant());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn state() -> Shared {
        Rc::new(RefCell::new(AppState::new()))
    }

    #[test]
    fn display_name_for_untitled_and_named_documents() {
        let s = state();
        assert_eq!(display_name_of(&s), "未命名");
        s.borrow_mut().path = Some(PathBuf::from("/tmp/笔记.txt"));
        assert_eq!(display_name_of(&s), "笔记.txt", "标签只显示文件名，不显示整条路径");
    }

    #[test]
    fn dirty_document_gets_dot_prefix() {
        let s = state();
        s.borrow_mut().path = Some(PathBuf::from("/tmp/a.txt"));
        s.borrow_mut().doc = Document::untitled().ok();
        if let Some(d) = s.borrow_mut().doc.as_mut() {
            d.dirty = true;
        }
        assert_eq!(display_name_of(&s), "● a.txt", "有未保存修改时标题带 ●");
    }
}
