//! 主窗口：标签栏 + 菜单 + 跨标签动作。
//!
//! 菜单布局对齐 Kate / EmEditor / Notepad-- 的通用结构：
//! 文件 / 编辑 / 视图 / 搜索 / 转到 / 工具 / 设置 / 帮助。
//! GTK4 已移除 GtkMenuBar / GtkToolbar，这里用 GtkPopoverMenuBar + GMenu +
//! GAction，工具栏用带 `.owl-toolbar` 样式类的 GtkBox。
//!
//! 一窗多标签：每个标签页是一份独立文档（[`Tab`]），菜单动作先交给当前
//! 标签页处理，未处理的（新建 / 打开 / 关闭 / 换主题…）由本文件兜底。
//! 多窗口：同一个 `GtkApplication` 下可以开任意多个窗口，主题与「最近打开
//! 文件」历史在窗口之间共享。

use crate::ui::recent::RecentFiles;
use crate::ui::state::{APP_ID, APP_NAME};
use crate::ui::tab::{self, Tab, TabEnv};
use crate::ui::theme::{self, Theme};
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};

pub struct Win {
    app: gtk::Application,
    window: gtk::ApplicationWindow,
    notebook: gtk::Notebook,
    tabs: RefCell<Vec<Rc<Tab>>>,
    /// 「当前是哪个标签页」的权威答案。
    ///
    /// 不能每次都问 `notebook.current_page()`：`switch-page` 信号发出时
    /// `cur_page` 尚未落定，而切页过程中会多次刷新界面（标签页的 hook 里也会
    /// 重设标题），只要有一处回头查 `current_page()`，就会把标题覆盖回上一页。
    /// 所以切页时先把这里改好，之后所有「当前页」查询都以它为准。
    active: RefCell<Option<Rc<Tab>>>,
    actions: gio::SimpleActionGroup,
    provider: gtk::CssProvider,
    theme: RefCell<Theme>,
    recent: RefCell<RecentFiles>,
    recent_menu: gio::Menu,
}

thread_local! {
    /// 活着的窗口（**强**引用）。
    ///
    /// 这里必须是强引用：GTK 只持有 `GtkApplicationWindow`（GObject），并不持有
    /// 我们的 `Rc<Win>`。若注册表也存弱引用，`open_window()` 一返回 `Rc<Win>`
    /// 就被 drop —— 窗口照样显示，但所有菜单动作回调里的 `weak.upgrade()` 都得到
    /// `None`，于是「打开」「打开最近文件」点了毫无反应。销毁时由 [`unregister`]
    /// 摘掉这一项，否则窗口对象永远不会释放。
    static WINDOWS: RefCell<Vec<Rc<Win>>> = const { RefCell::new(Vec::new()) };
}

fn register(w: &Rc<Win>) {
    WINDOWS.with(|v| v.borrow_mut().push(w.clone()));
}

/// 窗口销毁后注销，避免强引用让窗口（以及整份文档）常驻内存。
fn unregister(window: &gtk::ApplicationWindow) {
    WINDOWS.with(|v| v.borrow_mut().retain(|w| w.window != *window));
}

fn each_window(f: impl Fn(&Rc<Win>)) {
    let list: Vec<Rc<Win>> = WINDOWS.with(|v| v.borrow().clone());
    for w in &list {
        f(w);
    }
}

const ACTIONS: &[&str] = &[
    "new", "new_window", "open", "reload", "save", "save_as", "print", "close", "quit",
    "next_tab", "prev_tab",
    "undo", "redo", "cut", "copy", "paste", "select_all", "delete_line",
    "duplicate_line", "toggle_readonly", "find", "replace", "find_next", "find_prev",
    "toggle_line_numbers", "toggle_wrap", "zoom_in", "zoom_out", "zoom_reset",
    "fullscreen", "theme_light", "theme_dark", "goto", "doc_start", "doc_end",
    "word_count", "line_ending_lf", "line_ending_crlf", "toggle_status",
    "shortcuts", "about",
];
const STATEFUL: &[&str] = &[
    "toggle_readonly", "toggle_line_numbers", "toggle_wrap", "toggle_status",
];

fn menu_item(label: &str, action: &str, accel: &str) -> gio::MenuItem {
    let item = gio::MenuItem::new(Some(label), Some(&format!("win.{action}")));
    if !accel.is_empty() {
        item.set_attribute_value("accel", Some(&accel.to_variant()));
    }
    item
}

fn add(menu: &gio::Menu, label: &str, action: &str, accel: &str) {
    menu.append_item(&menu_item(label, action, accel));
}

fn section(menu: &gio::Menu, items: &[(&str, &str, &str)]) {
    let sec = gio::Menu::new();
    for (l, a, k) in items {
        add(&sec, l, a, k);
    }
    menu.append_section(None, &sec);
}

/// 最近打开文件的持久化位置：$XDG_CONFIG_HOME/owleditor/recent.txt。
fn recent_path() -> PathBuf {
    let mut dir = glib::user_config_dir();
    dir.push("owleditor");
    dir.push("recent.txt");
    dir
}

/// 用当前历史重建「打开最近文件」子菜单。空时放一个禁用占位项。
/// 标签显示文件名，action 目标值（参数）为完整绝对路径，确保打开的是正确文件。
fn fill_recent(menu: &gio::Menu, entries: &[PathBuf]) {
    menu.remove_all();
    if entries.is_empty() {
        // 占位项必须挂一个 action：GtkPopoverMenuBar 建树时会丢弃「既无 action
        // 又无子菜单」的条目，那样空历史时整条「打开最近文件」又会被判定为空而不显示。
        let empty = gio::MenuItem::new(Some("（无最近文件）"), Some("win.recent_empty"));
        menu.append_item(&empty);
        return;
    }
    for p in entries {
        let display = p
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| p.to_string_lossy().to_string());
        let target = p.to_string_lossy().to_string();
        let item = gio::MenuItem::new(Some(&display), Some("win.open_recent"));
        item.set_action_and_target_value(Some("win.open_recent"), Some(&target.to_variant()));
        menu.append_item(&item);
    }
}

fn build_menu(recent_menu: &gio::Menu) -> (gtk::PopoverMenuBar, gio::SimpleActionGroup) {
    let root = gio::Menu::new();

    let file = gio::Menu::new();
    let file_top = gio::Menu::new();
    add(&file_top, "新建标签页", "new", "<Control>n");
    add(&file_top, "新建窗口", "new_window", "<Control><Shift>n");
    add(&file_top, "打开…", "open", "<Control>o");
    // 紧跟在「打开」之后，符合用户要求的位置。
    file_top.append_submenu(Some("打开最近文件"), recent_menu);
    add(&file_top, "重新载入", "reload", "F5");
    file.append_section(None, &file_top);
    section(&file, &[
        ("保存", "save", "<Control>s"),
        ("另存为…", "save_as", "<Control><Shift>s"),
        ("打印…", "print", "<Control>p"),
    ]);
    section(&file, &[
        ("关闭标签页", "close", "<Control>w"),
        ("退出", "quit", "<Control>q"),
    ]);
    root.append_submenu(Some("文件"), &file);

    let edit = gio::Menu::new();
    section(&edit, &[
        ("撤销", "undo", "<Control>z"),
        ("重做", "redo", "<Control>y"),
    ]);
    section(&edit, &[
        ("剪切", "cut", "<Control>x"),
        ("复制", "copy", "<Control>c"),
        ("粘贴", "paste", "<Control>v"),
        ("全选", "select_all", "<Control>a"),
    ]);
    section(&edit, &[
        ("删除当前行", "delete_line", "<Control>d"),
        ("复制当前行", "duplicate_line", "<Control><Shift>l"),
        ("只读模式", "toggle_readonly", ""),
    ]);
    section(&edit, &[
        ("查找", "find", "<Control>f"),
        ("替换", "replace", "<Control>h"),
        ("查找下一个", "find_next", "F3"),
        ("查找上一个", "find_prev", "<Shift>F3"),
    ]);
    root.append_submenu(Some("编辑"), &edit);

    let view_menu = gio::Menu::new();
    section(&view_menu, &[
        ("显示行号", "toggle_line_numbers", ""),
        ("自动换行", "toggle_wrap", ""),
    ]);
    section(&view_menu, &[
        ("放大字体", "zoom_in", "<Control>plus"),
        ("缩小字体", "zoom_out", "<Control>minus"),
        ("重置缩放", "zoom_reset", "<Control>0"),
        ("全屏", "fullscreen", "F11"),
    ]);
    section(&view_menu, &[
        ("浅色主题", "theme_light", ""),
        ("深色主题", "theme_dark", ""),
    ]);
    root.append_submenu(Some("视图"), &view_menu);

    let search_menu = gio::Menu::new();
    section(&search_menu, &[
        ("查找", "find", "<Control>f"),
        ("替换", "replace", "<Control>h"),
        ("查找下一个", "find_next", "F3"),
        ("查找上一个", "find_prev", "<Shift>F3"),
        ("转到行…", "goto", "<Control>g"),
    ]);
    root.append_submenu(Some("搜索"), &search_menu);

    let goto = gio::Menu::new();
    section(&goto, &[
        ("转到行…", "goto", "<Control>g"),
        ("文件开头", "doc_start", "<Control>Home"),
        ("文件结尾", "doc_end", "<Control>End"),
    ]);
    root.append_submenu(Some("转到"), &goto);

    let tools = gio::Menu::new();
    section(&tools, &[("字数统计", "word_count", "")]);
    section(&tools, &[
        ("转换为 LF（Unix）", "line_ending_lf", ""),
        ("转换为 CRLF（Windows）", "line_ending_crlf", ""),
    ]);
    root.append_submenu(Some("工具"), &tools);

    let settings = gio::Menu::new();
    section(&settings, &[
        ("放大字体", "zoom_in", ""),
        ("缩小字体", "zoom_out", ""),
        ("显示状态栏", "toggle_status", ""),
    ]);
    root.append_submenu(Some("设置"), &settings);

    let help = gio::Menu::new();
    section(&help, &[
        ("快捷键一览", "shortcuts", ""),
        ("关于 OwlEditor", "about", ""),
    ]);
    root.append_submenu(Some("帮助"), &help);

    let group = gio::SimpleActionGroup::new();
    for name in ACTIONS {
        let action = if STATEFUL.contains(name) {
            // 初值必须与 AppState::new() 的默认一致，否则第一次打开菜单时
            // 「自动换行」显示未勾选、实际却已开启，点一下反而把它关掉。
            let initial = matches!(
                *name,
                "toggle_line_numbers" | "toggle_wrap" | "toggle_status"
            );
            gio::SimpleAction::new_stateful(*name, None, &initial.to_variant())
        } else {
            gio::SimpleAction::new(*name, None)
        };
        group.add_action(&action);
    }
    // 「（无最近文件）」占位项的哑动作：永久禁用。存在的意义只是让该条目
    // 成为一个「有 action 的 item」，从而能被 PopoverMenuBar 渲染出来。
    let empty_action = gio::SimpleAction::new("recent_empty", None);
    empty_action.set_enabled(false);
    group.add_action(&empty_action);
    (gtk::PopoverMenuBar::from_model(Some(&root)), group)
}

/// 头部工具按钮：图标 + 中文 tooltip。
// 用户明确要求：右侧不要 新建/打开/保存/剪切/复制/粘贴。
const TOOLS: &[(&str, &str, &str)] = &[
    ("edit-undo-symbolic", "撤销", "undo"),
    ("edit-redo-symbolic", "重做", "redo"),
    ("system-search-symbolic", "查找 (Ctrl+F)", "find"),
    ("edit-find-replace-symbolic", "替换 (Ctrl+H)", "replace"),
    ("zoom-in-symbolic", "放大", "zoom_in"),
    ("zoom-out-symbolic", "缩小", "zoom_out"),
    ("view-fullscreen-symbolic", "全屏", "fullscreen"),
];

fn tool_button(icon: &str, label: &str, action: &'static str, win: &Weak<Win>) -> gtk::Button {
    let b = gtk::Button::new();
    let image = gtk::Image::from_icon_name(icon);
    image.set_pixel_size(16);
    b.set_child(Some(&image));
    b.set_tooltip_text(Some(label));
    b.add_css_class("flat");
    b.add_css_class("owl-tool");
    b.set_can_focus(false);
    let w = win.clone();
    b.connect_clicked(move |_| {
        let name = action;
        if let Some(w) = w.upgrade() {
            dispatch_in(&w, name);
        }
    });
    b
}

/// 主题是应用级的（CSS 装在 display 上）：所有窗口一起换，避免
/// 「这个窗口浅色、那个窗口深色，编辑区与标题栏还对不上」。
fn set_theme(_w: &Rc<Win>, light: bool) {
    let t = if light { Theme::light() } else { Theme::dark() };
    let Some(display) = gdk::Display::default() else { return; };
    each_window(|w| {
        theme::install(&display, &w.provider, &t);
        *w.theme.borrow_mut() = t.clone();
        for tab in w.tabs.borrow().iter() {
            tab.state.borrow_mut().theme = t.clone();
            tab.view.queue_draw();
        }
    });
}

/// 窗口标题文案（纯函数，便于单测）。
pub fn window_title(name: &str, count: usize) -> String {
    if count > 1 {
        format!("{} — {}（{} 个标签页）", name, APP_NAME, count)
    } else {
        format!("{} — {}", name, APP_NAME)
    }
}

impl Win {
    fn active_tab(&self) -> Option<Rc<Tab>> {
        // 先认切换时记下的权威值（切页过程中 current_page() 尚不可信）。
        let cached = { self.active.borrow().clone() };
        if let Some(t) = cached {
            let alive = self.tabs.borrow().iter().any(|x| x.page == t.page);
            if alive {
                return Some(t);
            }
        }
        // 没有有效缓存（首次、或刚关掉该页）才回退问 notebook。
        let cur = self.notebook.current_page()?;
        self.tabs
            .borrow()
            .iter()
            .find(|t| self.notebook.page_num(&t.page) == Some(cur))
            .cloned()
    }

    /// 按「哪一页」定位标签页，而不是回头问 `current_page()`。
    ///
    /// `switch-page` 信号在部分 GTK 版本里早于 `cur_page` 落定，
    /// 此时 `current_page()` 仍是上一页；用信号自带的页面参数才没有歧义。
    fn tab_for_page(&self, page: &gtk::Widget) -> Option<Rc<Tab>> {
        self.tabs
            .borrow()
            .iter()
            .find(|t| t.page.upcast_ref::<gtk::Widget>() == page)
            .cloned()
    }

    fn sync_title_for(&self, tab: Option<&Rc<Tab>>) {
        let name = tab
            .map(|t| t.display_name())
            .unwrap_or_else(|| APP_NAME.to_string());
        let count = self.tabs.borrow().len();
        self.window.set_title(Some(&window_title(&name, count)));
    }

    fn sync_title(&self) {
        let tab = self.active_tab();
        self.sync_title_for(tab.as_ref());
    }

    /// 记进「最近打开文件」并刷新子菜单。
    fn remember(&self, path: &Path) {
        {
            let mut r = self.recent.borrow_mut();
            r.push(path);
            if let Err(e) = r.save(&recent_path()) {
                if let Some(t) = self.active_tab() {
                    t.state.borrow_mut().message = format!("记录最近文件失败: {e}");
                }
            }
        }
        let entries = self.recent.borrow().entries().to_vec();
        fill_recent(&self.recent_menu, &entries);
    }

    fn add_tab(self: &Rc<Self>, initial: Option<PathBuf>) -> Rc<Tab> {
        let weak = Rc::downgrade(self);
        let refresh_title = {
            let weak = weak.clone();
            Rc::new(move || {
                if let Some(w) = weak.upgrade() {
                    w.sync_title();
                }
            })
        };
        let remember_path = {
            let weak = weak.clone();
            Rc::new(move |p: &Path| {
                if let Some(w) = weak.upgrade() {
                    w.remember(p);
                }
            })
        };
        let window_action = {
            let weak = weak.clone();
            Rc::new(move |name: &str| {
                if let Some(w) = weak.upgrade() {
                    window_action_impl(&w, name);
                }
            })
        };
        let env = TabEnv {
            window: self.window.clone(),
            actions: self.actions.clone(),
            window_action,
            refresh_title,
            remember_path,
        };
        let tab = tab::build_tab(env, initial);
        // 主题跟当前窗口一致（浅色主题下新开的标签页不能又变回深色）。
        tab.state.borrow_mut().theme = self.theme.borrow().clone();
        // 真正打开的文件才记进「最近打开文件」（新建的空标签页不记）。
        let opened = { tab.state.borrow().path.clone() };
        if let Some(p) = opened {
            self.remember(&p);
        }

        // 标签：文件名 + 关闭按钮。
        let label = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        label.append(&tab.title);
        let close = gtk::Button::from_icon_name("window-close-symbolic");
        close.add_css_class("flat");
        close.add_css_class("owl-tab-close");
        close.set_can_focus(false);
        close.set_tooltip_text(Some("关闭标签页 (Ctrl+W)"));
        {
            let weak = weak.clone();
            let tab = Rc::downgrade(&tab);
            close.connect_clicked(move |_| {
                let (Some(w), Some(t)) = (weak.upgrade(), tab.upgrade()) else { return; };
                request_close_tab(&w, &t);
            });
        }
        label.append(&close);

        let idx = self.notebook.append_page(&tab.page, Some(&label));
        self.notebook.set_tab_reorderable(&tab.page, true);
        self.tabs.borrow_mut().push(tab.clone());
        *self.active.borrow_mut() = Some(tab.clone());
        self.notebook.set_current_page(Some(idx));
        self.sync_title();
        tab
    }

    /// 关闭一个标签页（调用前请完成未保存确认）。
    fn remove_tab(&self, tab: &Rc<Tab>) -> bool {
        if let Some(i) = self.notebook.page_num(&tab.page) {
            self.notebook.remove_page(Some(i));
        }
        // 先断开引用环、停掉后台线程，再丢弃这一页。
        tab.dispose();
        self.tabs.borrow_mut().retain(|t| t.page != tab.page);
        // 「当前页」缓存若指向被关掉的页，清掉它，让 active_tab() 回退到
        // notebook 的新当前页（删掉当前页时 GTK 会自动切到相邻页）。
        let stale = self
            .active
            .borrow()
            .as_ref()
            .map_or(false, |t| t.page == tab.page);
        if stale {
            *self.active.borrow_mut() = None;
        }
        // 先取出布尔值再动窗口：`if self.tabs.borrow().is_empty()` 的临时借用
        // 会活到整个 if 结束，而 close() 会同步回调 close_request 再读 tabs。
        let empty = { self.tabs.borrow().is_empty() };
        if empty {
            // 最后一个标签页关掉后关闭窗口（与 Kate 一致）。
            self.window.close();
            return true;
        }
        self.sync_title();
        if let Some(t) = self.active_tab() {
            t.focus();
        }
        false
    }
}

/// 菜单/快捷键/工具栏统一入口：先给当前标签页，再兜底给窗口动作。
fn dispatch_in(w: &Rc<Win>, name: &str) {
    if let Some(t) = w.active_tab() {
        if (t.dispatch)(name) {
            return;
        }
    }
    window_action_impl(w, name);
}

fn window_action_impl(w: &Rc<Win>, name: &str) {
    match name {
        "new" => {
            let t = w.add_tab(None);
            t.focus();
        }
        "new_window" => open_window(&w.app, Vec::new()),
        "open" => open_files_dialog(w),
        "close" => {
            if let Some(t) = w.active_tab() {
                request_close_tab(w, &t);
            }
        }
        "quit" => {
            // 关掉所有窗口；每个窗口都会走自己的未保存确认。
            let all: Vec<gtk::Window> = w.app.windows().into_iter().collect();
            for win in all {
                win.close();
            }
        }
        "next_tab" | "prev_tab" => {
            let n = w.notebook.n_pages() as i32;
            if n <= 1 {
                return;
            }
            let cur = w.notebook.current_page().unwrap_or(0) as i32;
            let step = if name == "next_tab" { 1 } else { -1 };
            let next = (cur + step).rem_euclid(n);
            w.notebook.set_current_page(Some(next as u32));
        }
        "theme_light" => set_theme(w, true),
        "theme_dark" => set_theme(w, false),
        "fullscreen" => {
            if w.window.is_fullscreen() {
                w.window.unfullscreen();
            } else {
                w.window.fullscreen();
            }
        }
        _ => {}
    }
}

fn open_files_dialog(w: &Rc<Win>) {
    let dlg = gtk::FileDialog::builder()
        .title("打开文件（可多选，支持 10 GiB 以上）")
        .modal(true)
        .build();
    let weak = Rc::downgrade(w);
    dlg.open_multiple(Some(&w.window), None::<&gio::Cancellable>, move |res| {
        let files = match res {
            Ok(f) => f,
            Err(e) => {
                // 用户取消是常态，静默即可；其余错误必须说出来，
                // 否则表现就是「点了打开什么都不发生」。
                if e.matches(gtk::DialogError::Dismissed) {
                    return;
                }
                if let Some(w) = weak.upgrade() {
                    if let Some(t) = w.active_tab() {
                        t.state.borrow_mut().message = format!("打开文件失败：{e}");
                        (t.hook)();
                    }
                }
                return;
            }
        };
        let Some(w) = weak.upgrade() else { return; };
        let mut paths = Vec::new();
        for i in 0..files.n_items() {
            if let Some(f) = files.item(i).and_then(|o| o.downcast::<gio::File>().ok()) {
                if let Some(p) = f.path() {
                    paths.push(p);
                }
            }
        }
        open_paths(&w, paths);
    });
}

/// 打开一批文件：如果当前是「干净的未命名空标签页」就先复用它，
/// 其余每个文件各开一个标签页。
fn open_paths(w: &Rc<Win>, paths: Vec<PathBuf>) {
    let mut paths = paths;
    if paths.is_empty() {
        return;
    }
    if let Some(t) = w.active_tab() {
        if t.is_pristine() {
            // 先 clone 再试：载入失败时文件必须留在 paths 里，
            // 否则 `remove(0)` 已把它吃掉，剩下的循环也不会再碰它。
            let first = paths[0].clone();
            if t.load_file(first).is_ok() {
                paths.remove(0);
                let p = t.state.borrow().path.clone();
                if let Some(p) = p {
                    w.remember(&p);
                }
                if paths.is_empty() {
                    t.focus();
                    return;
                }
            }
        }
    }
    let mut last = None;
    for p in paths {
        let t = w.add_tab(Some(p));
        last = Some(t);
    }
    if let Some(t) = last {
        t.focus();
    }
    w.sync_title();
}

fn request_close_tab(w: &Rc<Win>, tab: &Rc<Tab>) {
    if !tab.is_dirty() {
        w.remove_tab(tab);
        return;
    }
    let dlg = gtk::AlertDialog::builder()
        .message(format!("「{}」有未保存的修改", tab.display_name()))
        .detail("关闭前是否保存？选择「取消」可继续编辑。")
        .buttons(["取消", "不保存", "保存"])
        .cancel_button(0)
        .default_button(2)
        .build();
    let weak = Rc::downgrade(w);
    let t = tab.clone();
    dlg.choose(Some(&w.window), None::<&gio::Cancellable>, move |res| {
        let Some(w) = weak.upgrade() else { return; };
        match res {
            Ok(1) => {
                w.remove_tab(&t);
            }
            Ok(2) => {
                let weak2 = Rc::downgrade(&w);
                let t2 = t.clone();
                t.save(
                    false,
                    Rc::new(move || {
                        if let Some(w) = weak2.upgrade() {
                            w.remove_tab(&t2);
                        }
                    }),
                    // 取消保存：保持标签页，不需要额外动作。
                    Rc::new(|| {}),
                );
            }
            _ => {}
        }
    });
}

/// 关闭窗口时逐个确认未保存的标签页；全部确认后才真正销毁窗口。
fn confirm_close_queue(w: &Rc<Win>, queue: Rc<RefCell<Vec<Rc<Tab>>>>, guard: Rc<Cell<bool>>) {
    let next = {
        let mut q = queue.borrow_mut();
        if q.is_empty() {
            None
        } else {
            Some(q.remove(0))
        }
    };
    let Some(tab) = next else {
        guard.set(false);
        w.window.destroy();
        return;
    };
    if !tab.is_dirty() {
        confirm_close_queue(w, queue, guard);
        return;
    }
    let dlg = gtk::AlertDialog::builder()
        .message(format!("「{}」有未保存的修改", tab.display_name()))
        .detail("关闭窗口前是否保存？选择「取消」可继续编辑。")
        .buttons(["取消", "不保存", "保存"])
        .cancel_button(0)
        .default_button(2)
        .build();
    let weak = Rc::downgrade(w);
    let q = queue.clone();
    let g = guard.clone();
    dlg.choose(Some(&w.window), None::<&gio::Cancellable>, move |res| {
        let Some(w) = weak.upgrade() else { return; };
        match res {
            // 不保存：跳到下一个。
            Ok(1) => confirm_close_queue(&w, q, g),
            // 保存：成功后再继续（取消保存则整体中止，窗口保留）。
            Ok(2) => {
                let weak2 = Rc::downgrade(&w);
                let q2 = q.clone();
                let g2 = g.clone();
                let g3 = g.clone();
                tab.save(
                    false,
                    Rc::new(move || {
                        if let Some(w) = weak2.upgrade() {
                            confirm_close_queue(&w, q2.clone(), g2.clone());
                        }
                    }),
                    // 保存被取消 / 失败：整体中止，并复位闸门，
                    // 否则用户之后再点关闭会被静默吞掉（guard 一直为 true）。
                    Rc::new(move || g3.set(false)),
                );
            }
            // 取消：保持窗口，复位闸门。
            _ => g.set(false),
        }
    });
}

/// 新建一个窗口（可带初始文件，每个文件一个标签页）。
pub fn open_window(app: &gtk::Application, files: Vec<PathBuf>) {
    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .title(APP_NAME)
        .default_width(1180)
        .default_height(780)
        .build();
    window.set_icon_name(Some(APP_ID));
    window.add_css_class("owleditor");

    // ---------------------------------------------------------------- 主题
    // 新窗口沿用当前主题（用户切过浅色后，再开窗口不应又跳回深色）。
    let theme0 = {
        let mut t = Theme::dark();
        WINDOWS.with(|v| {
            if let Some(w) = v.borrow().first() {
                t = w.theme.borrow().clone();
            }
        });
        t
    };
    let provider = gtk::CssProvider::new();
    if let Some(display) = gdk::Display::default() {
        theme::install(&display, &provider, &theme0);
    }

    // ---------------------------------------------------------------- 最近打开文件
    // 顺序很关键：必须在 build_menu 之前把历史读进来并清掉已丢失的文件，这样
    // 「打开最近文件」子菜单在 PopoverMenuBar 绑定模型时就已经是「非空」的。
    // 空子菜单会被 GTK 直接跳过，之后再填条目也补不回来。
    let initial_recent = {
        let mut r = RecentFiles::from_file(&recent_path());
        let lost = r.prune_missing();
        if !lost.is_empty() {
            let _ = r.save(&recent_path());
        }
        r
    };
    let recent_menu = gio::Menu::new();
    fill_recent(&recent_menu, initial_recent.entries());

    // ---------------------------------------------------------------- 头部（菜单 + 工具按钮合一）
    let (menubar, actions) = build_menu(&recent_menu);
    menubar.add_css_class("owl-chrome");

    let notebook = gtk::Notebook::new();
    notebook.add_css_class("owl-tabs");
    notebook.set_scrollable(true);
    notebook.set_vexpand(true);
    notebook.set_hexpand(true);

    let win = Rc::new(Win {
        app: app.clone(),
        window: window.clone(),
        notebook: notebook.clone(),
        tabs: RefCell::new(Vec::new()),
        active: RefCell::new(None),
        actions,
        provider,
        theme: RefCell::new(theme0),
        recent: RefCell::new(initial_recent),
        recent_menu,
    });
    register(&win);
    window.insert_action_group("win", Some(&win.actions));

    // 「打开最近文件」动作：带一个字符串参数（文件绝对路径）。
    {
        let open_recent = gio::SimpleAction::new("open_recent", Some(&String::static_variant_type()));
        let weak = Rc::downgrade(&win);
        open_recent.connect_activate(move |_, param| {
            let Some(p) = param.and_then(|v| v.get::<String>()) else { return; };
            let Some(w) = weak.upgrade() else { return; };
            let path = PathBuf::from(p);
            if !path.is_file() {
                // 文件已丢失：提醒、并从历史中移除，刷新子菜单。
                w.recent.borrow_mut().remove(&path);
                let _ = w.recent.borrow().save(&recent_path());
                let entries = w.recent.borrow().entries().to_vec();
                fill_recent(&w.recent_menu, &entries);
                if let Some(t) = w.active_tab() {
                    t.state.borrow_mut().message =
                        format!("文件已不存在，已从最近打开中移除：{}", path.display());
                }
                let detail = format!(
                    "该文件已不存在，无法打开，已从「打开最近文件」中移除：\n{}",
                    path.display()
                );
                let dlg = gtk::AlertDialog::builder()
                    .message("最近文件已丢失")
                    .detail(detail)
                    .buttons(["确定"])
                    .default_button(0)
                    .build();
                dlg.choose(Some(&w.window), None::<&gio::Cancellable>, |_| {});
                return;
            }
            open_paths(&w, vec![path]);
        });
        win.actions.add_action(&open_recent);
    }

    // 「文件」菜单（PopoverMenuBar 的第一个子 popover）每次展开时做一次丢失检测：
    // 移除已不存在的条目并提醒，保证菜单里永远是可打开的有效文件。
    {
        let menubar = menubar.clone();
        let weak = Rc::downgrade(&win);
        glib::idle_add_local_once(move || {
            let mut child = menubar.first_child();
            while let Some(c) = child {
                if let Ok(pop) = c.clone().downcast::<gtk::PopoverMenu>() {
                    let weak = weak.clone();
                    pop.connect_notify_local(Some("visible"), move |pop, _| {
                        if !pop.is_visible() {
                            return;
                        }
                        let Some(w) = weak.upgrade() else { return; };
                        let removed = w.recent.borrow_mut().prune_missing();
                        if removed.is_empty() {
                            return;
                        }
                        let _ = w.recent.borrow().save(&recent_path());
                        let entries = w.recent.borrow().entries().to_vec();
                        fill_recent(&w.recent_menu, &entries);
                        if let Some(t) = w.active_tab() {
                            t.state.borrow_mut().message =
                                format!("已从最近打开移除 {} 个不存在的文件", removed.len());
                            (t.hook)();
                        }
                    });
                    break; // 第一个 popover 即「文件」菜单
                }
                child = c.next_sibling();
            }
        });
    }

    let toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 2);
    toolbar.add_css_class("owl-toolbar");
    for (icon, label, action) in TOOLS {
        if matches!(*action, "find" | "zoom_in" | "fullscreen") {
            toolbar.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        }
        toolbar.append(&tool_button(icon, label, action, &Rc::downgrade(&win)));
    }

    let header = gtk::HeaderBar::new();
    header.add_css_class("owl-chrome");
    header.pack_start(&menubar);
    header.pack_end(&toolbar);
    window.set_titlebar(Some(&header));
    window.set_child(Some(&notebook));

    // ---------------------------------------------------------------- 动作接线
    // 带勾选状态的菜单项在 GTK4 里走 `change-state`，普通项走 `activate`；
    // 两个信号都接，并用一次性闸门去重，保证一次点击只切换一次。
    for name in ACTIONS {
        let Some(action) = win.actions.lookup_action(name) else { continue; };
        let Ok(simple) = action.downcast::<gio::SimpleAction>() else { continue; };
        let n = *name;
        let weak = Rc::downgrade(&win);
        if STATEFUL.contains(name) {
            let fired = Rc::new(Cell::new(false));
            let make_fire = {
                let weak = weak.clone();
                let fired = fired.clone();
                move || {
                    if fired.replace(true) {
                        return;
                    }
                    if let Some(w) = weak.upgrade() {
                        dispatch_in(&w, n);
                    }
                    let f = fired.clone();
                    glib::idle_add_local_once(move || f.set(false));
                }
            };
            let f1 = make_fire.clone();
            simple.connect_activate(move |_, _| f1());
            let f2 = make_fire;
            simple.connect_change_state(move |_, _| f2());
        } else {
            let weak = weak.clone();
            simple.connect_activate(move |_, _| {
                if let Some(w) = weak.upgrade() {
                    dispatch_in(&w, n);
                }
            });
        }
    }

    // 切换标签页：刷新动作勾选状态、窗口标题与焦点。
    {
        let weak = Rc::downgrade(&win);
        notebook.connect_switch_page(move |_, page, _num| {
            if let Some(w) = weak.upgrade() {
                // 必须以信号带来的页面为准：此处 `current_page()` 可能还是上一页。
                let target = w.tab_for_page(page);
                // 先落定「当前页」，再刷新界面 —— 因为刷新会回调到窗口层
                // （标签页 hook 里的 refresh_title），那时读到的必须是新页。
                *w.active.borrow_mut() = target.clone();
                w.sync_title_for(target.as_ref());
                if let Some(t) = target {
                    (t.hook)();
                    t.focus();
                }
            }
        });
    }

    // 窗口销毁时把仍打开的标签页一并释放（未手动关闭的标签页也要断开引用环）。
    {
        let weak = Rc::downgrade(&win);
        // 用 WeakRef 引用窗口自身：闭包由窗口持有，捕获强引用会形成引用环。
        let gwin = window.downgrade();
        window.connect_destroy(move |_| {
            if let Some(w) = weak.upgrade() {
                for t in w.tabs.borrow().iter() {
                    t.dispose();
                }
                w.tabs.borrow_mut().clear();
                *w.active.borrow_mut() = None;
            }
            if let Some(gwin) = gwin.upgrade() {
                unregister(&gwin);
            }
        });
    }

    // ---------------------------------------------------------------- 关闭窗口
    // 标题栏 ❌ / Alt+F4 / Ctrl+Q 都会走 close-request；只有存在未保存修改时
    // 才拦截，逐个标签页确认。
    {
        let weak = Rc::downgrade(&win);
        let guard = Rc::new(Cell::new(false));
        window.connect_close_request(move |_| {
            if guard.get() {
                return glib::Propagation::Stop; // 对话框已弹出，别再叠一个
            }
            let Some(w) = weak.upgrade() else { return glib::Propagation::Proceed; };
            let dirty: Vec<Rc<Tab>> = w
                .tabs
                .borrow()
                .iter()
                .filter(|t| t.is_dirty())
                .cloned()
                .collect();
            if dirty.is_empty() {
                return glib::Propagation::Proceed;
            }
            guard.set(true);
            confirm_close_queue(&w, Rc::new(RefCell::new(dirty)), guard.clone());
            glib::Propagation::Stop
        });
    }

    // ---------------------------------------------------------------- 应用级快捷键
    // GMenu 上的 accel 属性只用于菜单里显示，GTK4 不会自动把它变成全局绑定；
    // 焦点落在查找框 / 菜单栏上时编辑区收不到按键，所以这里注册到 Application。
    const GLOBAL_ACCELS: &[(&str, &[&str])] = &[
        ("new", &["<Control>n"]),
        ("new_window", &["<Control><Shift>n"]),
        ("open", &["<Control>o"]),
        ("save", &["<Control>s"]),
        ("save_as", &["<Control><Shift>s"]),
        ("close", &["<Control>w"]),
        ("quit", &["<Control>q"]),
        ("find", &["<Control>f"]),
        ("replace", &["<Control>h"]),
        ("next_tab", &["<Control>Tab", "<Control>Page_Down"]),
        ("prev_tab", &["<Control><Shift>Tab", "<Control>Page_Up"]),
    ];
    for (action, accels) in GLOBAL_ACCELS {
        app.set_accels_for_action(&format!("win.{action}"), accels);
    }

    // ---------------------------------------------------------------- 启动
    window.present();
    if files.is_empty() {
        let t = win.add_tab(None);
        t.focus();
    } else {
        for p in files {
            win.add_tab(Some(p));
        }
        if let Some(t) = win.active_tab() {
            t.focus();
        }
    }
    win.sync_title();

    let weak = Rc::downgrade(&win);
    glib::idle_add_local_once(move || {
        if let Some(w) = weak.upgrade() {
            if let Some(t) = w.active_tab() {
                (t.hook)();
                t.focus();
            }
        }
    });
}


#[cfg(test)]
mod tests {
    use super::window_title;
    use crate::ui::state::APP_NAME;

    #[test]
    fn title_uses_file_name_of_active_tab() {
        assert_eq!(window_title("笔记.txt", 1), format!("笔记.txt — {APP_NAME}"));
    }

    #[test]
    fn title_counts_tabs_only_when_several_are_open() {
        assert_eq!(
            window_title("a.txt", 2),
            format!("a.txt — {APP_NAME}（2 个标签页）")
        );
        assert_eq!(
            window_title("a.txt", 5),
            format!("a.txt — {APP_NAME}（5 个标签页）")
        );
    }
}
