//! 主界面：左侧歌单 / 右侧封面+歌词 / 底部播放条。

pub mod art;
pub mod dialogs;
pub mod theme;

use crate::config::Config;
use crate::library::{self, Track};
use crate::lrc::{self, Lyrics};
use crate::player::{self, Player, Shared};
use crate::tags;
use gtk::prelude::*;
use gtk::{gdk, gio, glib, pango};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::time::{Duration, Instant};

// ---------- 固定高度（逻辑像素） ----------
// 顶栏与播放条是"外壳"，高度必须恒定；只有中间的歌单 / 歌词区随窗口伸缩。
/// 顶栏总高
pub const HEADER_H: i32 = 54;
/// 播放条总高
pub const BAR_H: i32 = 76;
/// 播放条上下内边距（与 theme.rs 里 `.playerbar` 的 padding 保持一致）
pub const BAR_PAD_Y: i32 = 6;
/// 播放条内容区高度（left 的封面 52 居中，center 的按钮 46 + 进度条 16 正好填满）
pub const BAR_CONTENT_H: i32 = BAR_H - BAR_PAD_Y * 2;
/// 播放条封面边长
const BAR_COVER: i32 = 52;
/// 舞台大封面边长
const STAGE_COVER: i32 = 168;

#[derive(Clone, Default)]
pub struct RowData {
    pub idx: String,
    pub title: String,
    pub sub: String,
    pub dur: String,
    pub playing: bool,
    pub selected: bool,
}

pub struct App {
    pub window: gtk::ApplicationWindow,
    pub cfg: Rc<RefCell<Config>>,
    pub player: Rc<Player>,
    pub shared: Arc<Shared>,
    pub provider: gtk::CssProvider,

    pub tracks: RefCell<Vec<Track>>,
    pub view: RefCell<Vec<usize>>,
    pub rows: Rc<RefCell<Vec<RowData>>>,
    /// 当前已绑定的行控件：列表位置 -> 行内 Box。行状态直接刷到这些控件上。
    pub cells: Rc<RefCell<HashMap<u32, gtk::Box>>>,
    pub current: Cell<Option<usize>>,
    pub lyrics: RefCell<Lyrics>,
    pub cur_line: Cell<Option<usize>>,
    pub queue: RefCell<Vec<usize>>,
    pub queue_pos: Cell<usize>,
    pub last_finished: Cell<u64>,
    pub last_dur: Cell<u64>,
    pub user_seek_at: Cell<Option<Instant>>,
    tick_count: Cell<u32>,
    /// 自动匹配的歌词结果（后台线程 -> 主线程）
    lyric_rx: RefCell<Option<Receiver<(PathBuf, Result<crate::online::LyricHit, String>)>>>,

    // 左侧
    pub store: gio::ListStore,
    pub selection: gtk::SingleSelection,
    pub list_view: gtk::ListView,
    pub search: gtk::SearchEntry,
    pub side_count: gtk::Label,
    pub status: gtk::Label,
    pub sidebar: gtk::Box,
    pub stage: gtk::Box,
    pub body: gtk::Box,

    // 右侧
    pub cover: gtk::Picture,
    /// 歌词区的模糊封面背景
    pub lyric_bg: gtk::Picture,
    /// 背景上的渐变遮罩
    pub lyric_scrim: gtk::Box,
    pub np_title: gtk::Label,
    pub np_artist: gtk::Label,
    pub np_sub: gtk::Label,
    pub lyric_box: gtk::Box,
    pub lyric_scroll: gtk::ScrolledWindow,
    pub lyric_labels: RefCell<Vec<gtk::Label>>,
    pub lyric_target: Cell<f64>,

    // 底部
    pub bar_title: gtk::Label,
    pub bar_artist: gtk::Label,
    pub bar_cover: gtk::Picture,
    pub play_img: gtk::Image,
    pub shuffle_btn: gtk::ToggleButton,
    pub repeat_btn: gtk::Button,
    pub pos_scale: gtk::Scale,
    pub pos_label: gtk::Label,
    pub dur_label: gtk::Label,
    pub vol_scale: gtk::Scale,
}

/// 把一行的状态写成 CSS 类。
///
/// 类只能挂在行内这个 Box（`.track-row`）上：列表的 `row` 节点在当前 GTK 里
/// 是内部控件，从 Rust 侧拿不到。所以主题里的选择器也必须以 `.track-row` 为根，
/// 写成 `.tracklist > row.playing` 永远不命中——这正是切歌后列表毫无反应的原因。
fn paint_row(row: &gtk::Box, data: &RowData) {
    for (class, on) in [("playing", data.playing), ("sel", data.selected)] {
        if on {
            row.add_css_class(class);
        } else {
            row.remove_css_class(class);
        }
    }
}

/// 把 `rows` 的状态重画到已绑定的行控件上。
fn paint_cells(cells: &HashMap<u32, gtk::Box>, rows: &[RowData]) {
    for (pos, row) in cells.iter() {
        if let Some(data) = rows.get(*pos as usize) {
            paint_row(row, data);
        }
    }
}

fn lbl(text: &str, class: &str) -> gtk::Label {
    let l = gtk::Label::new(Some(text));
    l.set_xalign(0.0);
    l.add_css_class(class);
    l
}

fn icon_btn(icon: &str, tip: &str) -> gtk::Button {
    let b = gtk::Button::new();
    b.set_child(Some(&gtk::Image::from_icon_name(icon)));
    b.set_tooltip_text(Some(tip));
    b.add_css_class("flat");
    b
}

fn icon_toggle(icon: &str, tip: &str) -> gtk::ToggleButton {
    let b = gtk::ToggleButton::new();
    b.set_child(Some(&gtk::Image::from_icon_name(icon)));
    b.set_tooltip_text(Some(tip));
    b.add_css_class("flat");
    b
}

/// 让内容坐标为 `line_y`、高度为 `line_h` 的那一行停在视口中线，需要的滚动值。
///
/// 视口顶部在内容坐标 `target` 处，行的中线在 `line_y + line_h/2`，
/// 两者相差正好半个视口高，行就居中了。
fn lyric_scroll_target(view_h: f64, line_y: f64, line_h: f64, offset: f64) -> f64 {
    (line_y + line_h / 2.0 - view_h / 2.0 + offset).max(0.0)
}

impl App {
    pub fn new(gtk_app: &gtk::Application) -> Rc<Self> {
        let cfg = Rc::new(RefCell::new(Config::load()));
        let (vol, w, h, theme_name, accent, shuffle_on, repeat, bg_opacity) = {
            let c = cfg.borrow();
            (
                c.volume,
                c.win_w,
                c.win_h,
                c.theme.clone(),
                c.accent.clone(),
                c.shuffle,
                c.repeat,
                c.lyric_bg_opacity,
            )
        };
        let dark = theme_name != "light";

        let provider = gtk::CssProvider::new();
        provider.load_from_string(&theme::css(&accent, dark));
        if let Some(disp) = gdk::Display::default() {
            gtk::style_context_add_provider_for_display(
                &disp,
                &provider,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
        }
        if let Some(s) = gtk::Settings::default() {
            s.set_gtk_application_prefer_dark_theme(dark);
        }

        let player = Rc::new(Player::new(vol));
        let shared = player.shared();

        // ---------- 窗口 ----------
        let window = gtk::ApplicationWindow::builder()
            .application(gtk_app)
            .title("owlmusic")
            .default_width(w)
            .default_height(h)
            .build();
        window.add_css_class("owl");

        let header = gtk::HeaderBar::new();
        header.add_css_class("owl-header");
        header.set_show_title_buttons(true);
        // 顶栏固定高度：内容永远不超过 HEADER_H - padding，窗口怎么拉都不变
        header.set_size_request(-1, HEADER_H);
        header.set_valign(gtk::Align::Start);
        let title_box = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        title_box.set_valign(gtk::Align::Center);
        title_box.set_size_request(-1, HEADER_H - 12);
        let logo = gtk::Image::from_icon_name("audio-x-generic-symbolic");
        logo.set_pixel_size(22);
        logo.set_valign(gtk::Align::Center);
        let title_txt = gtk::Box::new(gtk::Orientation::Vertical, 0);
        title_txt.set_valign(gtk::Align::Center);
        title_txt.set_size_request(-1, HEADER_H - 12);
        let app_title = lbl("owlmusic", "app-title");
        app_title.set_single_line_mode(true);
        app_title.set_ellipsize(pango::EllipsizeMode::End);
        let app_sub = lbl("本地音乐播放器", "app-sub");
        app_sub.set_single_line_mode(true);
        app_sub.set_ellipsize(pango::EllipsizeMode::End);
        title_txt.append(&app_title);
        title_txt.append(&app_sub);
        title_box.append(&logo);
        title_box.append(&title_txt);
        header.set_title_widget(Some(&title_box));

        let btn_add = icon_btn("folder-new-symbolic", "添加音乐文件夹");
        let btn_rescan = icon_btn("view-refresh-symbolic", "重新扫描曲库");
        let btn_set = icon_btn("emblem-system-symbolic", "设置");
        let right = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        right.append(&btn_add);
        right.append(&btn_rescan);
        right.append(&btn_set);
        header.pack_end(&right);
        window.set_titlebar(Some(&header));

        // ---------- 歌单（虚拟化列表） ----------
        let rows: Rc<RefCell<Vec<RowData>>> = Rc::new(RefCell::new(Vec::new()));
        let cells: Rc<RefCell<HashMap<u32, gtk::Box>>> = Rc::new(RefCell::new(HashMap::new()));
        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        let selection = gtk::SingleSelection::new(Some(store.clone()));
        selection.set_autoselect(false);
        selection.set_can_unselect(true);

        // 行内右键菜单需要访问 App，而 factory 早于 App 创建，用槽位连接
        let app_slot: Rc<RefCell<Option<Rc<App>>>> = Rc::new(RefCell::new(None));

        let factory = gtk::SignalListItemFactory::new();
        {
            let rows_setup = rows.clone();
            let app_slot = app_slot.clone();
            factory.connect_setup(move |_, item| {
                let item = item.downcast_ref::<gtk::ListItem>().unwrap();
                let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
                row.add_css_class("track-row");
                let idx = lbl("", "track-idx");
                idx.set_width_chars(2);
                idx.set_valign(gtk::Align::Center);
                let col = gtk::Box::new(gtk::Orientation::Vertical, 1);
                col.set_hexpand(true);
                col.set_valign(gtk::Align::Center);
                let title = lbl("", "track-title");
                title.set_ellipsize(pango::EllipsizeMode::End);
                title.set_max_width_chars(30);
                let sub = lbl("", "track-sub");
                sub.set_ellipsize(pango::EllipsizeMode::End);
                sub.set_max_width_chars(34);
                col.append(&title);
                col.append(&sub);
                let dur = lbl("", "track-dur");
                dur.set_valign(gtk::Align::Center);
                row.append(&idx);
                row.append(&col);
                row.append(&dur);
                item.set_child(Some(&row));
                let click = gtk::GestureClick::new();
                click.set_button(3);
                {
                    let slot = app_slot.clone();
                    let li = item.clone();
                    let row_c = row.clone();
                    click.connect_pressed(move |_, _, x, y| {
                        let pos = li.position();
                        if pos == u32::MAX {
                            return;
                        }
                        if let Some(a) = slot.borrow().clone() {
                            show_track_menu(a, pos as usize, row_c.upcast_ref(), x, y);
                        }
                    });
                }
                row.add_controller(click);
                let _ = &rows_setup;
            });
        }
        {
            let rows_bind = rows.clone();
            let cells_bind = cells.clone();
            factory.connect_bind(move |_, item| {
                let item = item.downcast_ref::<gtk::ListItem>().unwrap();
                let Some(obj) = item.item() else { return };
                let Ok(obj) = obj.downcast::<glib::BoxedAnyObject>() else {
                    return;
                };
                let pos = *obj.borrow::<usize>();
                let data = rows_bind.borrow().get(pos).cloned().unwrap_or_default();
                let Some(child) = item.child() else { return };
                let Ok(row) = child.downcast::<gtk::Box>() else {
                    return;
                };
                let mut it = row.first_child();
                let mut parts: Vec<gtk::Widget> = Vec::new();
                while let Some(w) = it {
                    let next = w.next_sibling();
                    parts.push(w);
                    it = next;
                }
                if let Some(w) = parts.first() {
                    if let Ok(l) = w.clone().downcast::<gtk::Label>() {
                        l.set_text(&data.idx);
                    }
                }
                if let Some(col) = parts.get(1) {
                    let mut c = col.first_child();
                    let mut labels = Vec::new();
                    while let Some(w) = c {
                        let next = w.next_sibling();
                        labels.push(w);
                        c = next;
                    }
                    if let Some(w) = labels.first() {
                        if let Ok(l) = w.clone().downcast::<gtk::Label>() {
                            l.set_text(&data.title);
                        }
                    }
                    if let Some(w) = labels.get(1) {
                        if let Ok(l) = w.clone().downcast::<gtk::Label>() {
                            l.set_text(&data.sub);
                        }
                    }
                }
                if let Some(w) = parts.get(2) {
                    if let Ok(l) = w.clone().downcast::<gtk::Label>() {
                        l.set_text(&data.dur);
                    }
                }
                paint_row(&row, &data);
                cells_bind.borrow_mut().insert(pos as u32, row.clone());
            });
        }
        {
            // 行被回收前注销并清干净，免得复用到别的曲目上还留着「正在播放」的样子
            let cells_unbind = cells.clone();
            factory.connect_unbind(move |_, item| {
                let Some(li) = item.downcast_ref::<gtk::ListItem>() else {
                    return;
                };
                let Some(child) = li.child() else { return };
                let Ok(row) = child.downcast::<gtk::Box>() else {
                    return;
                };
                row.remove_css_class("playing");
                row.remove_css_class("sel");
                cells_unbind.borrow_mut().retain(|_, v| *v != row);
            });
        }

        let list_view = gtk::ListView::new(Some(selection.clone()), Some(factory.clone()));
        list_view.add_css_class("tracklist");
        list_view.set_single_click_activate(true);
        list_view.set_show_separators(false);

        let list_scroll = gtk::ScrolledWindow::new();
        list_scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        list_scroll.set_child(Some(&list_view));
        list_scroll.set_vexpand(true);

        let search = gtk::SearchEntry::new();
        search.set_placeholder_text(Some("搜索曲名 / 艺人 / 专辑"));
        search.add_css_class("owl-search");
        search.set_hexpand(true);

        let side_count = lbl("", "side-count");
        let head = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        head.add_css_class("side-head");
        head.append(&lbl("歌单", "side-title"));
        head.append(&side_count);

        let search_wrap = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        search_wrap.set_margin_start(14);
        search_wrap.set_margin_end(14);
        search_wrap.set_margin_bottom(8);
        search_wrap.append(&search);

        let status = lbl("", "hint");
        status.set_margin_start(18);
        status.set_margin_end(14);
        status.set_margin_bottom(10);
        status.set_wrap(true);
        status.set_max_width_chars(46);

        let sidebar = gtk::Box::new(gtk::Orientation::Vertical, 0);
        sidebar.add_css_class("sidebar");
        sidebar.set_width_request(392);
        sidebar.append(&head);
        sidebar.append(&search_wrap);
        sidebar.append(&list_scroll);
        sidebar.append(&status);

        // ---------- 右侧舞台 ----------
        // 封面用定尺纹理（见 art::square）：Picture 的 natural size 等于纹理尺寸，
        // 不裁成 STAGE_COVER 的话，一张 1000px 的图会把整个信息头撑高。
        let cover = gtk::Picture::new();
        cover.set_content_fit(gtk::ContentFit::Cover);
        cover.set_size_request(STAGE_COVER, STAGE_COVER);
        let cover_frame = gtk::Box::new(gtk::Orientation::Vertical, 0);
        cover_frame.add_css_class("cover-frame");
        cover_frame.set_size_request(STAGE_COVER, STAGE_COVER);
        cover_frame.set_valign(gtk::Align::Center);
        cover_frame.append(&cover);

        let np_title = lbl("未在播放", "np-title");
        np_title.set_ellipsize(pango::EllipsizeMode::End);
        np_title.set_max_width_chars(26);
        let np_artist = lbl("从左侧选一首歌开始", "np-artist");
        np_artist.set_ellipsize(pango::EllipsizeMode::End);
        let np_sub = lbl("", "np-sub");
        np_sub.set_ellipsize(pango::EllipsizeMode::End);

        let np_info = gtk::Box::new(gtk::Orientation::Vertical, 7);
        np_info.set_valign(gtk::Align::Center);
        np_info.set_hexpand(true);
        np_info.append(&np_title);
        np_info.append(&np_artist);
        np_info.append(&np_sub);

        let np_header = gtk::Box::new(gtk::Orientation::Horizontal, 22);
        np_header.append(&cover_frame);
        np_header.append(&np_info);

        let lyric_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        lyric_box.set_valign(gtk::Align::Start);
        lyric_box.set_margin_top(28);
        lyric_box.set_margin_bottom(28);

        let lyric_scroll = gtk::ScrolledWindow::new();
        lyric_scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        lyric_scroll.set_child(Some(&lyric_box));
        lyric_scroll.set_vexpand(true);
        lyric_scroll.set_kinetic_scrolling(false);

        // ---------- 歌词区背景：专辑封面 → 模糊 → 叠在歌词下面 ----------
        // 注意放在 Overlay 的 overlay 层（不是 child）：overlay 子项不参与尺寸计算，
        // 否则 Picture 的 natural size 会把歌词区撑大。
        let lyric_bg = gtk::Picture::new();
        lyric_bg.set_content_fit(gtk::ContentFit::Cover);
        lyric_bg.set_hexpand(true);
        lyric_bg.set_vexpand(true);
        lyric_bg.set_opacity(bg_opacity as f64);

        // 压一层渐变遮罩，保证亮色封面上歌词也读得清
        let lyric_scrim = gtk::Box::new(gtk::Orientation::Vertical, 0);
        lyric_scrim.add_css_class("lyric-scrim");
        lyric_scrim.set_hexpand(true);
        lyric_scrim.set_vexpand(true);

        let lyric_area = gtk::Overlay::new();
        lyric_area.add_css_class("lyric-area");
        lyric_area.set_overflow(gtk::Overflow::Hidden); // 让圆角真正裁掉背景
        lyric_area.set_hexpand(true);
        lyric_area.set_vexpand(true);
        let area_fill = gtk::Box::new(gtk::Orientation::Vertical, 0);
        area_fill.set_hexpand(true);
        area_fill.set_vexpand(true);
        lyric_area.set_child(Some(&area_fill));
        lyric_area.add_overlay(&lyric_bg);
        lyric_area.add_overlay(&lyric_scrim);
        lyric_area.add_overlay(&lyric_scroll);

        let lyric_tools = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        lyric_tools.append(&lbl("歌词", "side-title"));
        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.set_hexpand(true);
        lyric_tools.append(&spacer);
        let btn_online_lyric = icon_btn("system-search-symbolic", "在线搜索歌词");
        let btn_save_lyric = icon_btn("document-save-symbolic", "把歌词写入文件标签");
        let btn_lyric_style = icon_btn("preferences-desktop-font-symbolic", "歌词样式");
        lyric_tools.append(&btn_online_lyric);
        lyric_tools.append(&btn_save_lyric);
        lyric_tools.append(&btn_lyric_style);

        let stage = gtk::Box::new(gtk::Orientation::Vertical, 12);
        stage.add_css_class("stage");
        stage.set_hexpand(true);
        stage.set_vexpand(true);
        stage.append(&np_header);
        stage.append(&lyric_tools);
        stage.append(&lyric_area);

        let body = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        body.set_hexpand(true);
        body.set_vexpand(true);
        body.append(&sidebar);
        body.append(&stage);

        // ---------- 播放条（固定高度，不随封面/歌词变化） ----------
        let bar_cover = gtk::Picture::new();
        bar_cover.set_content_fit(gtk::ContentFit::Cover);
        bar_cover.set_size_request(BAR_COVER, BAR_COVER);
        let bar_cover_frame = gtk::Box::new(gtk::Orientation::Vertical, 0);
        bar_cover_frame.add_css_class("cover-frame");
        bar_cover_frame.add_css_class("small");
        bar_cover_frame.set_overflow(gtk::Overflow::Hidden);
        bar_cover_frame.set_size_request(BAR_COVER, BAR_COVER);
        bar_cover_frame.set_halign(gtk::Align::Start);
        bar_cover_frame.set_valign(gtk::Align::Center);
        bar_cover_frame.append(&bar_cover);

        let bar_title = lbl("未在播放", "bar-title");
        bar_title.set_ellipsize(pango::EllipsizeMode::End);
        bar_title.set_max_width_chars(20);
        let bar_artist = lbl("", "bar-sub");
        bar_artist.set_ellipsize(pango::EllipsizeMode::End);
        bar_artist.set_max_width_chars(20);
        let bar_text = gtk::Box::new(gtk::Orientation::Vertical, 1);
        bar_text.set_valign(gtk::Align::Center);
        bar_text.append(&bar_title);
        bar_text.append(&bar_artist);

        let bar_left = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        bar_left.set_valign(gtk::Align::Center);
        bar_left.append(&bar_cover_frame);
        bar_left.append(&bar_text);

        let shuffle_btn = icon_toggle("media-playlist-shuffle-symbolic", "随机播放");
        shuffle_btn.set_active(shuffle_on);
        let btn_prev = icon_btn("media-skip-backward-symbolic", "上一首");
        let play_btn = gtk::Button::new();
        play_btn.add_css_class("play-pill");
        let play_img = gtk::Image::from_icon_name("media-playback-start-symbolic");
        play_img.set_pixel_size(20);
        play_btn.set_child(Some(&play_img));
        play_btn.set_size_request(46, 46);
        play_btn.set_tooltip_text(Some("播放 / 暂停 (空格)"));
        let btn_next = icon_btn("media-skip-forward-symbolic", "下一首");
        let repeat_btn = icon_btn("media-playlist-repeat-symbolic", "循环模式：列表循环");
        if repeat == 2 {
            repeat_btn.set_child(Some(&gtk::Image::from_icon_name(
                "media-playlist-repeat-song-symbolic",
            )));
        }
        if repeat != 0 {
            repeat_btn.add_css_class("checked");
        }

        let transport = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        transport.set_halign(gtk::Align::Center);
        transport.set_valign(gtk::Align::Center);
        transport.set_size_request(-1, 46);
        transport.append(&shuffle_btn);
        transport.append(&btn_prev);
        transport.append(&play_btn);
        transport.append(&btn_next);
        transport.append(&repeat_btn);

        let pos_scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 1000.0, 1.0);
        pos_scale.add_css_class("progress");
        pos_scale.set_draw_value(false);
        pos_scale.set_hexpand(true);
        pos_scale.set_valign(gtk::Align::Center);
        pos_scale.set_size_request(-1, 14);
        let pos_label = lbl("0:00", "time");
        pos_label.set_xalign(1.0);
        pos_label.set_valign(gtk::Align::Center);
        let dur_label = lbl("0:00", "time");
        dur_label.set_xalign(0.0);
        dur_label.set_valign(gtk::Align::Center);

        let prog = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        prog.set_size_request(-1, 16);
        prog.append(&pos_label);
        prog.append(&pos_scale);
        prog.append(&dur_label);

        // 46(按钮) + 2(间距) + 16(进度) = BAR_CONTENT_H，填满不多不少
        let center = gtk::Box::new(gtk::Orientation::Vertical, 2);
        center.set_hexpand(true);
        center.set_width_request(420);
        center.set_size_request(-1, BAR_CONTENT_H);
        center.set_valign(gtk::Align::Center);
        center.append(&transport);
        center.append(&prog);

        let vol_img = gtk::Image::from_icon_name("audio-volume-high-symbolic");
        let vol_scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 1.0, 0.01);
        vol_scale.add_css_class("vol");
        vol_scale.set_draw_value(false);
        vol_scale.set_size_request(96, -1);
        vol_scale.set_valign(gtk::Align::Center);
        vol_scale.set_value(vol as f64);
        let btn_only_lyric = icon_btn("view-list-symbolic", "只看歌词");
        let bar_right = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        bar_right.set_valign(gtk::Align::Center);
        bar_right.append(&vol_img);
        bar_right.append(&vol_scale);
        bar_right.append(&btn_only_lyric);

        let centerbox = gtk::CenterBox::new();
        centerbox.add_css_class("playerbar");
        // 固定高度：底栏是外壳，不参与弹性分配。
        // 总高 BAR_H 减去 CSS 的上下 padding，就是内容区必须占的高度。
        centerbox.set_size_request(-1, BAR_H - BAR_PAD_Y * 2);
        centerbox.set_valign(gtk::Align::End);
        centerbox.set_start_widget(Some(&bar_left));
        centerbox.set_center_widget(Some(&center));
        centerbox.set_end_widget(Some(&bar_right));

        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.append(&body);
        root.append(&centerbox);
        window.set_child(Some(&root));

        let app = Rc::new(App {
            window: window.clone(),
            cfg: cfg.clone(),
            player: player.clone(),
            shared: shared.clone(),
            provider,
            tracks: RefCell::new(Vec::new()),
            view: RefCell::new(Vec::new()),
            rows: rows.clone(),
            cells: cells.clone(),
            current: Cell::new(None),
            lyrics: RefCell::new(Lyrics::default()),
            cur_line: Cell::new(None),
            queue: RefCell::new(Vec::new()),
            queue_pos: Cell::new(0),
            last_finished: Cell::new(shared.finished.load(Ordering::Relaxed)),
            last_dur: Cell::new(0),
            user_seek_at: Cell::new(None),
            tick_count: Cell::new(0),
            lyric_rx: RefCell::new(None),
            store: store.clone(),
            selection: selection.clone(),
            list_view: list_view.clone(),
            search: search.clone(),
            side_count: side_count.clone(),
            status: status.clone(),
            sidebar: sidebar.clone(),
            stage: stage.clone(),
            body: body.clone(),
            cover: cover.clone(),
            lyric_bg: lyric_bg.clone(),
            lyric_scrim: lyric_scrim.clone(),
            np_title: np_title.clone(),
            np_artist: np_artist.clone(),
            np_sub: np_sub.clone(),
            lyric_box: lyric_box.clone(),
            lyric_scroll: lyric_scroll.clone(),
            lyric_labels: RefCell::new(Vec::new()),
            lyric_target: Cell::new(0.0),
            bar_title: bar_title.clone(),
            bar_artist: bar_artist.clone(),
            bar_cover: bar_cover.clone(),
            play_img: play_img.clone(),
            shuffle_btn: shuffle_btn.clone(),
            repeat_btn: repeat_btn.clone(),
            pos_scale: pos_scale.clone(),
            pos_label: pos_label.clone(),
            dur_label: dur_label.clone(),
            vol_scale: vol_scale.clone(),
        });

        *app_slot.borrow_mut() = Some(app.clone());

        if !crate::alsa::available() {
            app.set_status("没有找到 libasound，播放会失败：请安装 libasound2");
        }

        app.apply_layout();
        app.apply_cover_bg();

        // ================= 事件接线 =================
        {
            let a = app.clone();
            app.list_view.clone().connect_activate(move |_, pos| {
                if let Some(&idx) = a.view.borrow().get(pos as usize) {
                    a.play_index(idx, true);
                }
            });
        }
        {
            let a = app.clone();
            app.selection.clone().connect_selection_changed(move |_, _, _| {
                a.refresh_row_marks();
            });
        }
        {
            let a = app.clone();
            app.search.clone().connect_search_changed(move |e| {
                let q = e.text().to_string();
                a.apply_filter(&q);
            });
        }

        {
            let a = app.clone();
            play_btn.clone().connect_clicked(move |_| {
                a.toggle_play();
            });
        }
        {
            let a = app.clone();
            btn_prev.clone().connect_clicked(move |_| a.prev_track());
        }
        {
            let a = app.clone();
            btn_next.clone().connect_clicked(move |_| a.next_track(true));
        }
        {
            let a = app.clone();
            app.pos_scale.clone().connect_change_value(move |_, _, v| {
                a.user_seek_at.set(Some(Instant::now()));
                let dur = a.shared.dur();
                if dur > 0 {
                    a.player.seek((v as u64).min(dur));
                }
                glib::Propagation::Proceed
            });
        }
        {
            let a = app.clone();
            app.vol_scale.clone().connect_change_value(move |_, _, v| {
                a.player.set_volume(v as f32);
                {
                    let mut c = a.cfg.borrow_mut();
                    c.volume = v as f32;
                    c.save();
                }
                glib::Propagation::Proceed
            });
        }
        {
            let a = app.clone();
            app.shuffle_btn.clone().connect_toggled(move |b| {
                {
                    let mut c = a.cfg.borrow_mut();
                    c.shuffle = b.is_active();
                    c.save();
                }
                a.rebuild_queue();
            });
        }
        {
            let a = app.clone();
            app.repeat_btn.clone().connect_clicked(move |btn| {
                let mode = {
                    let mut c = a.cfg.borrow_mut();
                    c.repeat = (c.repeat + 1) % 3;
                    c.save();
                    c.repeat
                };
                let (icon, tip) = match mode {
                    0 => ("media-playlist-repeat-symbolic", "循环模式：关闭"),
                    1 => ("media-playlist-repeat-symbolic", "循环模式：列表循环"),
                    _ => ("media-playlist-repeat-song-symbolic", "循环模式：单曲循环"),
                };
                btn.set_child(Some(&gtk::Image::from_icon_name(icon)));
                btn.set_tooltip_text(Some(tip));
                if mode == 0 {
                    btn.remove_css_class("checked");
                } else {
                    btn.add_css_class("checked");
                }
            });
        }
        {
            let a = app.clone();
            btn_add.clone().connect_clicked(move |_| {
                let dlg = gtk::FileDialog::builder()
                    .title("选择音乐文件夹")
                    .modal(true)
                    .build();
                let a2 = a.clone();
                let win = a.window.clone();
                dlg.select_folder(
                    Some(&win),
                    None::<&gio::Cancellable>,
                    move |res| {
                        if let Ok(file) = res {
                            if let Some(p) = file.path() {
                                let need = {
                                    let mut c = a2.cfg.borrow_mut();
                                    if c.roots.contains(&p) {
                                        false
                                    } else {
                                        c.roots.push(p.clone());
                                        c.save();
                                        true
                                    }
                                };
                                if need {
                                    a2.rescan();
                                }
                            }
                        }
                    },
                );
            });
        }
        {
            let a = app.clone();
            btn_rescan.clone().connect_clicked(move |_| a.rescan());
        }
        {
            let a = app.clone();
            btn_set.clone().connect_clicked(move |_| dialogs::open_settings(a.clone()));
        }
        {
            let a = app.clone();
            let btn = btn_lyric_style.clone();
            btn_lyric_style.clone().connect_clicked(move |_| {
                dialogs::open_lyric_style(a.clone(), &btn);
            });
        }
        {
            let a = app.clone();
            btn_online_lyric.clone().connect_clicked(move |_| {
                dialogs::fetch_online_lyric(a.clone());
            });
        }
        {
            let a = app.clone();
            btn_save_lyric.clone().connect_clicked(move |_| {
                let Some(t) = a.current_track() else { return };
                let lyr = a.lyrics.borrow().clone();
                if lyr.is_empty() {
                    a.set_status("当前没有可写入的歌词");
                    return;
                }
                let text = if lyr.synced { lyr.to_lrc() } else { lyr.raw.clone() };
                let side = t.path.with_extension("lrc");
                let mut ok = 0;
                if std::fs::write(&side, &text).is_ok() {
                    ok += 1;
                }
                match tags::write_lyrics(&t.path, &text) {
                    Ok(()) => ok += 1,
                    Err(e) => a.set_status(&format!("写入标签失败：{e}")),
                }
                if let Some(cur) = a.current.get() {
                    a.refresh_track(cur);
                }
                if ok > 0 {
                    a.set_status("歌词已写入标签与同名 .lrc 文件");
                }
            });
        }
        {
            let a = app.clone();
            btn_only_lyric.clone().connect_clicked(move |_| {
                let vis = a.lyric_scroll.is_visible();
                a.lyric_scroll.set_visible(!vis);
                // 歌词藏起来时，背景也别孤零零留一块糊图
                let bg_on = !vis && a.cfg.borrow().cover_bg;
                a.lyric_bg.set_visible(bg_on);
                a.lyric_scrim.set_visible(bg_on);
            });
        }

        // 快捷键
        {
            let a = app.clone();
            let key = gtk::EventControllerKey::new();
            key.connect_key_pressed(move |_, keyval, _, _| {
                use gdk::Key;
                match keyval {
                    Key::space => {
                        a.toggle_play();
                        glib::Propagation::Stop
                    }
                    Key::Right => {
                        a.seek_relative(5_000);
                        glib::Propagation::Stop
                    }
                    Key::Left => {
                        a.seek_relative(-5_000);
                        glib::Propagation::Stop
                    }
                    _ => glib::Propagation::Proceed,
                }
            });
            window.add_controller(key);
        }

        // 关闭时保存窗口尺寸
        {
            let a = app.clone();
            window.connect_close_request(move |w| {
                let (ww, hh) = (w.default_width(), w.default_height());
                {
                    let pos = a.shared.pos();
                    let mut c = a.cfg.borrow_mut();
                    c.win_w = ww;
                    c.win_h = hh;
                    if a.shared.dur() > 0 {
                        c.last_pos_ms = pos;
                    }
                    c.save();
                }
                glib::Propagation::Proceed
            });
        }

        app.clone().start_tick();
        app.clone().initial_load();
        app
    }

    fn initial_load(self: Rc<Self>) {
        // 加载歌词样式后再启动扫描
        self.restyle_lyrics();
        let has_roots = !self.cfg.borrow().roots.is_empty();
        if has_roots {
            self.rescan();
        } else {
            self.set_status("点右上角文件夹按钮，选择你的音乐目录");
        }
    }

    // ================= 列表数据 =================
    pub fn apply_filter(&self, q: &str) {
        let q = q.trim().to_lowercase();
        let tracks = self.tracks.borrow();
        let mut view = Vec::new();
        for (i, t) in tracks.iter().enumerate() {
            let hit = q.is_empty()
                || t.title.to_lowercase().contains(&q)
                || t.artist.to_lowercase().contains(&q)
                || t.album.to_lowercase().contains(&q);
            if hit {
                view.push(i);
            }
        }
        drop(tracks);
        *self.view.borrow_mut() = view;
        self.side_count
            .set_text(&format!("{} 首", self.view.borrow().len()));
        self.rebuild_rows();
    }

    fn rebuild_rows(&self) {
        let view = self.view.borrow().clone();
        let tracks = self.tracks.borrow();
        let cur = self.current.get();
        let sel_pos = self.selection.selected();
        let mut data = Vec::with_capacity(view.len());
        for (pos, &i) in view.iter().enumerate() {
            let Some(t) = tracks.get(i) else { continue };
            data.push(RowData {
                idx: format!("{}", (t.track_no.unwrap_or(pos as u32 + 1))),
                title: t.title.clone(),
                sub: t.display_sub(),
                dur: library::fmt_time(t.dur_ms),
                playing: cur == Some(i),
                selected: sel_pos == pos as u32,
            });
        }
        drop(tracks);
        *self.rows.borrow_mut() = data;
        self.store.remove_all();
        for pos in 0..self.view.borrow().len() {
            self.store.append(&glib::BoxedAnyObject::new(pos));
        }
    }

    /// 只刷新行状态（正在播放 / 选中），不动列表数据。
    ///
    /// 先把状态写回数据模型，保证之后任何一次重绑（滚动、筛选、重排）
    /// 拿到的都是新值；再直接把类刷到已经绑定的行控件上。
    /// 这里刻意不再用 `items_changed` 骗列表重绑：数据源根本没变，
    /// 重绑既浪费又可能被列表吞掉，症状就是切歌后高亮还是上一首。
    fn refresh_row_marks(&self) {
        let cur = self.current.get();
        let sel_pos = self.selection.selected();
        {
            let view = self.view.borrow();
            let mut rows = self.rows.borrow_mut();
            for (pos, &i) in view.iter().enumerate() {
                if let Some(r) = rows.get_mut(pos) {
                    r.playing = cur == Some(i);
                    r.selected = sel_pos == pos as u32;
                }
            }
        }
        self.paint_bound_rows();
    }

    /// 把 `rows` 里的状态重画到所有已绑定的行上（可见行，滚动时随绑随画）。
    fn paint_bound_rows(&self) {
        paint_cells(&self.cells.borrow(), &self.rows.borrow());
    }

    pub fn set_status(&self, s: &str) {
        self.status.set_text(s);
    }

    // ================= 播放控制 =================
    pub fn toggle_play(&self) {
        match self.shared.state() {
            player::ST_PLAYING => self.player.pause(),
            player::ST_PAUSED => self.player.play(),
            _ => {
                if let Some(cur) = self.current.get() {
                    self.play_index(cur, true);
                } else if let Some(&first) = self.view.borrow().first() {
                    self.play_index(first, true);
                }
            }
        }
        self.update_play_button();
    }

    pub fn play_index(&self, idx: usize, autoplay: bool) {
        let track = match self.tracks.borrow().get(idx) {
            Some(t) => t.clone(),
            None => return,
        };
        self.current.set(Some(idx));
        self.player.load(track.path.clone(), track.dur_ms, autoplay);
        self.update_play_button();
        self.update_now_playing(&track);
        self.load_art(&track);
        self.load_lyrics(&track);
        self.rebuild_queue();
        {
            let mut c = self.cfg.borrow_mut();
            c.last_track = Some(track.path.clone());
            c.save();
        }
        if let Some(pos) = self.view.borrow().iter().position(|&i| i == idx) {
            self.selection.set_selected(pos as u32);
            self.list_view
                .scroll_to(pos as u32, gtk::ListScrollFlags::NONE, None);
        }
        self.refresh_row_marks();
    }

    /// 从当前列表移除（不动磁盘文件）
    pub fn remove_track(&self, idx: usize) {
        {
            let mut tracks = self.tracks.borrow_mut();
            if idx >= tracks.len() {
                return;
            }
            tracks.remove(idx);
        }
        match self.current.get() {
            Some(c) if c == idx => {
                self.player.stop();
                self.current.set(None);
            }
            Some(c) if c > idx => self.current.set(Some(c - 1)),
            _ => {}
        }
        let q = self.search.text().to_string();
        self.apply_filter(&q);
        library::save_cache(&self.tracks.borrow());
        self.set_status("已从列表移除，文件仍在磁盘上");
    }

    pub fn rebuild_queue(&self) {
        let cur = self.current.get();
        let shuffle = self.shuffle_btn.is_active();
        let view = self.view.borrow().clone();
        let mut q: Vec<usize> = if shuffle {
            use rand::seq::SliceRandom;
            let mut rest: Vec<usize> = view.iter().cloned().filter(|&i| Some(i) != cur).collect();
            rest.shuffle(&mut rand::thread_rng());
            let mut v = Vec::with_capacity(rest.len() + 1);
            if let Some(c) = cur {
                v.push(c);
            }
            v.extend(rest);
            v
        } else {
            view
        };
        if q.is_empty() {
            if let Some(c) = cur {
                q.push(c);
            }
        }
        let pos = cur.and_then(|c| q.iter().position(|&i| i == c)).unwrap_or(0);
        self.queue_pos.set(pos);
        *self.queue.borrow_mut() = q;
    }

    pub fn next_track(&self, by_user: bool) {
        let repeat = self.cfg.borrow().repeat;
        if !by_user && repeat == 2 {
            if let Some(cur) = self.current.get() {
                self.play_index(cur, true);
                return;
            }
        }
        let q = self.queue.borrow().clone();
        if q.is_empty() {
            return;
        }
        let pos = self.queue_pos.get();
        let next = if pos + 1 < q.len() {
            pos + 1
        } else if repeat >= 1 || by_user {
            0
        } else {
            self.player.stop();
            self.update_play_button();
            return;
        };
        self.queue_pos.set(next);
        if let Some(&idx) = q.get(next) {
            self.play_index(idx, true);
        }
    }

    pub fn prev_track(&self) {
        if self.shared.pos() > 3000 {
            self.player.seek(0);
            return;
        }
        let q = self.queue.borrow().clone();
        if q.is_empty() {
            return;
        }
        let pos = self.queue_pos.get();
        let prev = if pos > 0 { pos - 1 } else { q.len() - 1 };
        self.queue_pos.set(prev);
        if let Some(&idx) = q.get(prev) {
            self.play_index(idx, true);
        }
    }

    pub fn seek_relative(&self, delta_ms: i64) {
        let dur = self.shared.dur();
        let pos = self.shared.pos() as i64;
        let target = (pos + delta_ms).clamp(0, dur as i64) as u64;
        self.player.seek(target);
    }

    // ================= 界面刷新 =================
    fn update_now_playing(&self, t: &Track) {
        self.np_title.set_text(&t.title);
        self.np_artist.set_text(t.display_artist());
        let mut sub = vec![t.display_album().to_string()];
        if t.has_composer() {
            sub.push(format!("作曲 {}", t.composer));
        }
        if let Some(y) = t.year {
            sub.push(y.to_string());
        }
        sub.push(t.ext().to_uppercase());
        self.np_sub.set_text(&sub.join(" · "));
        self.bar_title.set_text(&t.title);
        self.bar_artist.set_text(t.display_artist());
        self.window
            .set_title(Some(&format!("{} — {}", t.title, t.display_artist())));
    }

    fn load_art(&self, t: &Track) {
        let blur = self.cfg.borrow().blur;
        match tags::read_cover(&t.path).and_then(|c| art::from_bytes(&c.data, blur)) {
            Some(a) => {
                // 两处封面各自用「定尺纹理」：Picture 的 natural size 就是纹理尺寸，
                // 直接把原图塞进去，一张 1000px 的封面会把播放条顶高。
                let big = art::square(&a.cover, STAGE_COVER as u32);
                let small = art::square(&a.cover, BAR_COVER as u32);
                self.cover.set_paintable(big.as_ref());
                self.bar_cover.set_paintable(small.as_ref());
                // 模糊封面只铺在歌词区，不透明度可配
                self.lyric_bg.set_paintable(Some(&a.blur));
                self.lyric_bg
                    .set_opacity(self.cfg.borrow().lyric_bg_opacity as f64);
                if self.cfg.borrow().accent_follow_cover {
                    let hex = art::hex(a.accent);
                    if hex != self.cfg.borrow().accent {
                        self.apply_accent(&hex);
                    }
                }
            }
            None => {
                self.cover.set_paintable(None::<&gdk::Texture>);
                self.bar_cover.set_paintable(None::<&gdk::Texture>);
                self.lyric_bg.set_paintable(None::<&gdk::Texture>);
            }
        }
    }

    pub fn apply_accent(&self, hex: &str) {
        {
            let mut c = self.cfg.borrow_mut();
            c.accent = hex.to_string();
            c.save();
        }
        self.reload_css();
    }

    /// 歌单左右切换
    pub fn apply_layout(&self) {
        let right = self.cfg.borrow().sidebar_right;
        if right {
            self.body
                .reorder_child_after(&self.sidebar, Some(&self.stage));
            self.sidebar.add_css_class("right");
        } else {
            self.body
                .reorder_child_after(&self.stage, Some(&self.sidebar));
            self.sidebar.remove_css_class("right");
        }
    }

    /// 是否用封面当歌词背景
    pub fn apply_cover_bg(&self) {
        let on = self.cfg.borrow().cover_bg;
        self.lyric_bg.set_visible(on);
        self.lyric_scrim.set_visible(on);
    }

    /// 只改背景不透明度，不用重新解码封面
    pub fn apply_bg_opacity(&self) {
        let o = self.cfg.borrow().lyric_bg_opacity as f64;
        self.lyric_bg.set_opacity(o);
    }

    /// 模糊强度变化后重新生成背景
    pub fn refresh_blur(&self) {
        if let Some(i) = self.current.get() {
            let t = self.tracks.borrow().get(i).cloned();
            if let Some(t) = t {
                self.load_art(&t);
            }
        }
    }

    pub fn reload_css(&self) {
        let (accent, dark) = {
            let c = self.cfg.borrow();
            (c.accent.clone(), c.theme != "light")
        };
        self.provider.load_from_string(&theme::css(&accent, dark));
    }

    // ================= 歌词 =================
    pub fn load_lyrics(&self, t: &Track) {
        let mut text: Option<String> = None;
        let side = t.path.with_extension("lrc");
        if let Ok(s) = std::fs::read_to_string(&side) {
            if !s.trim().is_empty() {
                text = Some(s);
            }
        }
        if text.is_none() {
            text = tags::read_lyrics(&t.path);
        }
        match text {
            Some(s) => self.set_lyrics(lrc::parse(&s)),
            None => {
                self.set_lyrics(Lyrics::default());
                if let Some(cached) = read_lyric_cache(&t.path) {
                    self.set_lyrics(lrc::parse(&cached));
                    return;
                }
                if self.cfg.borrow().auto_lyric {
                    self.set_status("没有内嵌歌词，正在自动在线匹配…");
                    let (tx, rx) =
                        std::sync::mpsc::channel::<(PathBuf, Result<crate::online::LyricHit, String>)>();
                    let t2 = t.clone();
                    std::thread::spawn(move || {
                        let r = crate::online::fetch_lyrics(
                            &t2.artist, &t2.title, &t2.album, t2.dur_ms,
                        );
                        let _ = tx.send((t2.path.clone(), r));
                    });
                    *self.lyric_rx.borrow_mut() = Some(rx);
                } else {
                    self.set_status("这首歌还没有歌词，点歌词栏右侧搜索按钮在线抓取");
                }
            }
        }
    }

    pub fn set_lyrics(&self, lyr: Lyrics) {
        while let Some(child) = self.lyric_box.first_child() {
            self.lyric_box.remove(&child);
        }
        self.lyric_labels.borrow_mut().clear();
        self.cur_line.set(None);
        if lyr.is_empty() {
            let empty = gtk::Label::new(Some("暂无歌词"));
            empty.add_css_class("np-sub");
            empty.set_margin_top(40);
            self.lyric_box.append(&empty);
            *self.lyrics.borrow_mut() = lyr;
            return;
        }
        let style = self.cfg.borrow().lyric.clone();
        let color = self.lyric_color();
        let synced = lyr.synced;
        let mut labels = Vec::new();
        for line in lyr.lines.iter() {
            if style.hide_empty && line.text.trim().is_empty() && line.trans.is_none() {
                continue;
            }
            let text = match (&line.trans, style.show_trans) {
                (Some(tr), true) => format!("{}\n{}", line.text, tr),
                _ => line.text.clone(),
            };
            let l = gtk::Label::new(Some(&text));
            l.set_wrap(true);
            l.set_wrap_mode(pango::WrapMode::WordChar);
            l.set_xalign(match style.align {
                0 => 0.0,
                2 => 1.0,
                _ => 0.5,
            });
            l.set_justify(match style.align {
                0 => gtk::Justification::Left,
                2 => gtk::Justification::Right,
                _ => gtk::Justification::Center,
            });
            l.set_margin_top((style.line_spacing / 2).max(2));
            l.set_margin_bottom((style.line_spacing / 2).max(2));
            l.set_margin_start(10);
            l.set_margin_end(10);
            self.apply_lyric_attrs(&l, &style, color, false);
            if synced {
                let player = self.player.clone();
                let time = line.time;
                let click = gtk::GestureClick::new();
                click.connect_released(move |_, _, _, _| {
                    player.seek(time);
                });
                l.add_controller(click);
                l.set_cursor_from_name(Some("pointer"));
            }
            self.lyric_box.append(&l);
            labels.push(l);
        }
        *self.lyrics.borrow_mut() = lyr;
        *self.lyric_labels.borrow_mut() = labels;
        // 立即定位到当前行
        let pos = self.shared.pos();
        if let Some(idx) = self.lyrics.borrow().index_at(pos) {
            self.highlight_line(Some(idx));
            self.update_lyric_scroll(false);
        }
    }

    fn lyric_color(&self) -> (u8, u8, u8) {
        let style = self.cfg.borrow().lyric.clone();
        match style.color.as_str() {
            "text" => {
                if self.cfg.borrow().theme == "light" {
                    (21, 23, 28)
                } else {
                    (237, 238, 243)
                }
            }
            "accent" => art::parse_hex(&self.cfg.borrow().accent),
            hex => art::parse_hex(hex),
        }
    }

    fn apply_lyric_attrs(
        &self,
        l: &gtk::Label,
        style: &crate::config::LyricStyle,
        color: (u8, u8, u8),
        active: bool,
    ) {
        let attrs = pango::AttrList::new();
        let size = if active {
            style.font_size as f64 * 1.16
        } else {
            style.font_size as f64
        };
        attrs.insert(pango::AttrSize::new((size as i32) * pango::SCALE));
        if active {
            attrs.insert(pango::AttrInt::new_weight(pango::Weight::Bold));
            let ac = art::parse_hex(&self.cfg.borrow().accent);
            attrs.insert(pango::AttrColor::new_foreground(
                (ac.0 as u16) * 257,
                (ac.1 as u16) * 257,
                (ac.2 as u16) * 257,
            ));
            l.set_opacity(1.0);
        } else {
            attrs.insert(pango::AttrInt::new_weight(pango::Weight::Normal));
            attrs.insert(pango::AttrColor::new_foreground(
                (color.0 as u16) * 257,
                (color.1 as u16) * 257,
                (color.2 as u16) * 257,
            ));
            l.set_opacity(style.dim.clamp(0.12, 1.0) as f64);
        }
        l.set_attributes(Some(&attrs));
    }

    pub fn restyle_lyrics(&self) {
        let lyr = self.lyrics.borrow().clone();
        if lyr.is_empty() {
            // 重建空状态样式
            if self.lyric_labels.borrow().is_empty() {
                return;
            }
        }
        self.set_lyrics(lyr);
    }

    /// 当前行必须停在歌词控件的中线上。
    ///
    /// 之前是按「估算行高」累加算每行 y：label 还没布局时 `height()` 是 0，
    /// 就退回 36px 估算。歌词字号调大后真实行高远超 36，每行都差几像素，
    /// 几十行累计下来当前行就一路往下漂，最后漂出视口。
    /// 现在直接问 GTK 要真实位置（`compute_bounds`），而且首尾各留半个视口，
    /// 保证第一行、最后一行也能停在中线。
    fn update_lyric_scroll(&self, animate: bool) {
        let Some(cur) = self.cur_line.get() else {
            return;
        };
        let labels = self.lyric_labels.borrow();
        if labels.is_empty() {
            return;
        }
        let view_h = self.lyric_scroll.height() as f64;
        if view_h <= 1.0 {
            return; // 还没完成布局，等下一帧 tick
        }

        // 上下各留半个视口，任何一行都能滚到正中（最后一行也不例外）
        let half = (view_h / 2.0).round();
        if (self.lyric_box.margin_top() as f64 - half).abs() > 0.5 {
            self.lyric_box.set_margin_top(half as i32);
            self.lyric_box.set_margin_bottom(half as i32);
        }

        let i = cur.min(labels.len() - 1);
        // compute_bounds 给的是相对 lyric_box 的坐标，不受滚动位置影响
        let Some(b) = labels[i].compute_bounds(&self.lyric_box) else {
            return;
        };
        let h = b.height() as f64;
        let offset = self.cfg.borrow().lyric.offset_px as f64;
        // 内容坐标 = 上边距 + box 内 y（margin 刚设成 half）
        let content_y = half + b.y() as f64;
        let target = lyric_scroll_target(view_h, content_y, h, offset);
        drop(labels);
        self.lyric_target.set(target);
        if !animate {
            let adj = self.lyric_scroll.vadjustment();
            adj.set_value(target);
        }
    }

    fn animate_scroll(&self) {
        let target = self.lyric_target.get();
        let adj = self.lyric_scroll.vadjustment();
        let cur = adj.value();
        let diff = target - cur;
        if diff.abs() > 0.7 {
            adj.set_value(cur + diff * 0.22);
        }
    }

    fn highlight_line(&self, idx: Option<usize>) {
        let labels = self.lyric_labels.borrow().clone();
        if labels.is_empty() {
            return;
        }
        let style = self.cfg.borrow().lyric.clone();
        let color = self.lyric_color();
        for (i, l) in labels.iter().enumerate() {
            self.apply_lyric_attrs(l, &style, color, Some(i) == idx);
        }
        self.cur_line.set(idx);
    }

    // ================= 定时刷新 =================
    fn start_tick(self: Rc<Self>) {
        let app = self.clone();
        glib::timeout_add_local(Duration::from_millis(33), move || {
            app.tick();
            glib::ControlFlow::Continue
        });
    }

    fn tick(&self) {
        let pos = self.shared.pos();
        let dur = self.shared.dur();
        if dur != self.last_dur.get() {
            self.last_dur.set(dur);
            self.pos_scale.adjustment().set_upper(dur.max(1) as f64);
            self.dur_label.set_text(&library::fmt_time(dur));
        }
        let user_recent = self
            .user_seek_at
            .get()
            .map(|t| t.elapsed() < Duration::from_millis(800))
            .unwrap_or(false);
        if !user_recent && dur > 0 {
            self.pos_scale.set_value(pos.min(dur) as f64);
        }
        self.pos_label.set_text(&library::fmt_time(pos));
        self.update_play_button();

        // 歌词跟随
        let (synced, cur_idx) = {
            let l = self.lyrics.borrow();
            (l.synced && !l.is_empty(), l.index_at(pos))
        };
        if synced {
            if cur_idx != self.cur_line.get() {
                self.highlight_line(cur_idx);
            }
            // 每帧校正一次：换行要回中线，窗口缩放/字号变化也要重新对齐
            self.update_lyric_scroll(true);
        }
        self.animate_scroll();

        // 每 ~4 秒记一次进度（不写盘，退出时统一保存）
        if self.shared.state() == player::ST_PLAYING {
            let n = self.tick_count.get().wrapping_add(1);
            self.tick_count.set(n);
            if n % 120 == 0 {
                let pos = self.shared.pos();
                self.cfg.borrow_mut().last_pos_ms = pos;
            }
        }

        // 自动匹配的歌词回来了
        let got = {
            let mut slot = self.lyric_rx.borrow_mut();
            match slot.as_ref().map(|rx| rx.try_recv()) {
                Some(Ok(msg)) => {
                    *slot = None;
                    Some(msg)
                }
                Some(Err(std::sync::mpsc::TryRecvError::Empty)) => None,
                Some(Err(_)) => {
                    *slot = None;
                    None
                }
                None => None,
            }
        };
        if let Some((path, res)) = got {
            let is_current = self
                .current
                .get()
                .and_then(|i| self.tracks.borrow().get(i).map(|t| t.path.clone()))
                .map(|p| p == path)
                .unwrap_or(false);
            if is_current {
                match res {
                    Ok(hit) => {
                        let lyr = lrc::parse(&hit.lrc);
                        if !lyr.is_empty() {
                            write_lyric_cache(&path, &hit.lrc);
                            self.set_lyrics(lyr);
                            self.set_status(&format!(
                                "已自动匹配歌词（{}）",
                                if hit.source.is_empty() {
                                    "在线"
                                } else {
                                    &hit.source
                                }
                            ));
                        }
                    }
                    Err(_) => self.set_status("没找到匹配歌词，可以手动在线搜索"),
                }
            }
        }

        if let Some(e) = self.shared.take_error() {
            self.set_status(&format!("播放出错：{e}"));
        }

        let fin = self.shared.finished.load(Ordering::Relaxed);
        if fin != self.last_finished.get() {
            self.last_finished.set(fin);
            self.next_track(false);
        }
    }

    pub fn update_play_button(&self) {
        let icon = if self.shared.state() == player::ST_PLAYING {
            "media-playback-pause-symbolic"
        } else {
            "media-playback-start-symbolic"
        };
        self.play_img.set_icon_name(Some(icon));
    }

    // ================= 曲库 =================
    pub fn rescan(self: &Rc<Self>) {
        let roots = self.cfg.borrow().roots.clone();
        if roots.is_empty() {
            self.set_status("还没有添加音乐文件夹，点右上角文件夹按钮添加");
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel::<ScanMsg>();
        let tx2 = tx.clone();
        std::thread::spawn(move || {
            let mut last_sent = 0usize;
            let tracks = library::scan(&roots, |n, p| {
                if n - last_sent >= 25 {
                    last_sent = n;
                    let _ =
                        tx2.send(ScanMsg::Progress(n, p.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()));
                }
            });
            let _ = tx2.send(ScanMsg::Done(Box::new(tracks)));
        });
        self.poll_scan(rx);
    }

    fn poll_scan(self: &Rc<Self>, rx: Receiver<ScanMsg>) {
        let app = self.clone();
        glib::timeout_add_local(Duration::from_millis(120), move || match rx.try_recv() {
            Ok(ScanMsg::Progress(n, name)) => {
                app.set_status(&format!("正在扫描 {n} 首… {name}"));
                glib::ControlFlow::Continue
            }
            Ok(ScanMsg::Done(tracks)) => {
                let n = tracks.len();
                *app.tracks.borrow_mut() = *tracks;
                let q = app.search.text().to_string();
                app.apply_filter(&q);
                app.set_status(&format!("曲库共 {n} 首 · 双击或回车播放"));
                if let Some(last) = app.cfg.borrow().last_track.clone() {
                    if let Some(idx) = app.tracks.borrow().iter().position(|t| t.path == last) {
                        app.current.set(Some(idx));
                        let t = app.tracks.borrow()[idx].clone();
                        app.update_now_playing(&t);
                        app.load_art(&t);
                        app.load_lyrics(&t);
                        app.player.load(t.path.clone(), t.dur_ms, false);
                        let (resume, pos) = {
                            let c = app.cfg.borrow();
                            (c.resume, c.last_pos_ms)
                        };
                        if resume && pos > 1500 && t.dur_ms > pos + 2000 {
                            app.player.seek(pos);
                            app.set_status(&format!(
                                "已停在上次的位置 {}，按空格继续",
                                library::fmt_time(pos)
                            ));
                        }
                        app.refresh_row_marks();
                    }
                }
                glib::ControlFlow::Break
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => glib::ControlFlow::Break,
        });
    }

    pub fn refresh_track(&self, idx: usize) {
        let path = match self.tracks.borrow().get(idx) {
            Some(t) => t.path.clone(),
            None => return,
        };
        if let Ok(t) = library::read_track(&path) {
            if let Some(slot) = self.tracks.borrow_mut().get_mut(idx) {
                *slot = t.clone();
            }
            if self.current.get() == Some(idx) {
                self.update_now_playing(&t);
            }
            library::save_cache(&self.tracks.borrow());
            self.rebuild_rows();
        }
    }

    pub fn current_track(&self) -> Option<Track> {
        self.current
            .get()
            .and_then(|i| self.tracks.borrow().get(i).cloned())
    }

    pub fn current_index(&self) -> Option<usize> {
        self.current.get()
    }

    // --- 供对话框调用的包装 ---
    pub fn lyric_color_pub(&self) -> (u8, u8, u8) {
        self.lyric_color()
    }

    pub fn update_now_playing_pub(&self, t: &Track) {
        self.update_now_playing(t);
    }

    pub fn reload_art_pub(&self, t: &Track) {
        self.load_art(t);
    }

    pub fn refresh_lyric_offset(&self) {
        self.update_lyric_scroll(true);
    }
}

// ================= 行右键菜单 =================
fn show_track_menu(app: Rc<App>, pos: usize, anchor: &gtk::Widget, x: f64, y: f64) {
    let pop = gtk::Popover::new();
    pop.add_css_class("owl-pop");
    pop.set_has_arrow(false);
    pop.set_parent(anchor);

    let col = gtk::Box::new(gtk::Orientation::Vertical, 2);
    pop.set_child(Some(&col));

    let mk = |text: &str, icon: &str| -> gtk::Button {
        let b = gtk::Button::new();
        b.add_css_class("flat");
        let hb = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        hb.append(&gtk::Image::from_icon_name(icon));
        let l = gtk::Label::new(Some(text));
        l.set_xalign(0.0);
        hb.append(&l);
        b.set_child(Some(&hb));
        b
    };
    let b_play = mk("播放", "media-playback-start-symbolic");
    let b_match = mk("在线匹配元数据…", "system-search-symbolic");
    let b_edit = mk("编辑标签…", "document-edit-symbolic");
    let b_reveal = mk("在文件夹中显示", "folder-open-symbolic");
    let b_remove = mk("从列表移除", "list-remove-symbolic");
    for b in [&b_play, &b_match, &b_edit, &b_reveal, &b_remove] {
        col.append(b);
    }

    {
        let a = app.clone();
        let p = pop.clone();
        b_play.connect_clicked(move |_| {
            p.popdown();
            if let Some(&idx) = a.view.borrow().get(pos) {
                a.play_index(idx, true);
            }
        });
    }
    {
        let a = app.clone();
        let p = pop.clone();
        b_match.connect_clicked(move |_| {
            p.popdown();
            if let Some(&idx) = a.view.borrow().get(pos) {
                a.play_index(idx, false);
                dialogs::open_match(a.clone());
            }
        });
    }
    {
        let a = app.clone();
        let p = pop.clone();
        b_edit.connect_clicked(move |_| {
            p.popdown();
            if let Some(&idx) = a.view.borrow().get(pos) {
                dialogs::open_edit_tags(a.clone(), Some(idx));
            }
        });
    }
    {
        let a = app.clone();
        let p = pop.clone();
        b_reveal.connect_clicked(move |_| {
            p.popdown();
            if let Some(&idx) = a.view.borrow().get(pos) {
                let path = a.tracks.borrow().get(idx).map(|t| t.path.clone());
                if let Some(path) = path {
                    if let Some(dir) = path.parent() {
                        let uri = gio::File::for_path(dir).uri();
                        let _ = gio::AppInfo::launch_default_for_uri(
                            &uri,
                            None::<&gio::AppLaunchContext>,
                        );
                    }
                }
            }
        });
    }
    {
        let a = app.clone();
        let p = pop.clone();
        b_remove.connect_clicked(move |_| {
            p.popdown();
            if let Some(&idx) = a.view.borrow().get(pos) {
                a.remove_track(idx);
            }
        });
    }

    pop.connect_closed(|p| p.unparent());
    pop.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
    pop.popup();
}

// ================= 歌词缓存 ================
fn lyric_cache_path(path: &std::path::Path) -> PathBuf {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut h);
    crate::config::cache_dir()
        .join("lyrics")
        .join(format!("{:016x}.lrc", h.finish()))
}

fn read_lyric_cache(path: &std::path::Path) -> Option<String> {
    std::fs::read_to_string(lyric_cache_path(path)).ok().filter(|s| !s.trim().is_empty())
}

fn write_lyric_cache(path: &std::path::Path, text: &str) {
    let p = lyric_cache_path(path);
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(p, text);
}

enum ScanMsg {
    Progress(usize, String),
    Done(Box<Vec<Track>>),
}

/// 后台执行任务，回到主线程处理结果（用于网络请求）
pub fn spawn_task<T, W, D>(work: W, done: D)
where
    T: Send + 'static,
    W: FnOnce() -> T + Send + 'static,
    D: FnOnce(T) + 'static,
{
    let (tx, rx) = std::sync::mpsc::channel::<T>();
    std::thread::spawn(move || {
        let v = work();
        let _ = tx.send(v);
    });
    let done = Rc::new(RefCell::new(Some(done)));
    glib::timeout_add_local(Duration::from_millis(90), move || match rx.try_recv() {
        Ok(v) => {
            if let Some(f) = done.borrow_mut().take() {
                f(v);
            }
            glib::ControlFlow::Break
        }
        Err(std::sync::mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
        Err(std::sync::mpsc::TryRecvError::Disconnected) => glib::ControlFlow::Break,
    });
}

#[allow(dead_code)]
pub fn write_temp_cover(cover: &tags::Cover, name: &str) -> Option<PathBuf> {
    tags::cover_to_temp(cover, name)
}

#[cfg(test)]
mod layout_tests {
    use super::*;

    /// 歌词背景是 overlay 层：它不能参与尺寸计算，
    /// 否则一张大封面会反过来把歌词区顶高，破坏「中间弹性」。
    #[test]
    #[ignore]
    fn lyric_bg_does_not_grow_area() {
        if !gtk::is_initialized() && gtk::init().is_err() {
            println!("跳过：没有可用的 display");
            return;
        }
        let b = glib::Bytes::from_owned(vec![0u8; (400 * 400 * 4) as usize]);
        let big: gdk::Texture =
            gdk::MemoryTexture::new(400, 400, gdk::MemoryFormat::R8g8b8a8, &b, 1600).upcast();

        let area = gtk::Overlay::new();
        let fill = gtk::Box::new(gtk::Orientation::Vertical, 0);
        fill.set_hexpand(true);
        fill.set_vexpand(true);
        area.set_child(Some(&fill));

        let pic = gtk::Picture::new();
        pic.set_content_fit(gtk::ContentFit::Cover);
        pic.set_hexpand(true);
        pic.set_vexpand(true);
        pic.set_paintable(Some(&big));
        area.add_overlay(&pic);

        let (min, nat, _, _) = area.measure(gtk::Orientation::Vertical, -1);
        println!("歌词区（含 400px 背景图）min={min} natural={nat}");
        assert_eq!(nat, 0, "背景图不能参与歌词区的尺寸计算");
    }

    /// 切歌后「正在播放」必须能落到行控件上：类挂在 .track-row，
    /// 且换一首时旧行要摘掉、新行戴上，不许残留两行都亮。
    #[test]
    #[ignore]
    fn row_classes_follow_current_track() {
        if !gtk::is_initialized() && gtk::init().is_err() {
            println!("跳过：没有可用的 display");
            return;
        }
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        row.add_css_class("track-row");

        paint_row(&row, &RowData { playing: true, ..Default::default() });
        assert!(row.has_css_class("playing"), "正在播放的行必须带 playing");

        paint_row(&row, &RowData::default());
        assert!(!row.has_css_class("playing"), "切歌后旧行必须摘掉 playing");
        let mine = row
            .css_classes()
            .iter()
            .filter(|c| *c == "playing" || *c == "sel")
            .count();
        assert_eq!(mine, 0, "状态类必须清空（GTK 自带的 horizontal 不算）");
    }

    /// 切歌时「正在播放」要从旧行移到新行：不能两行都亮，也不能一行都不亮。
    /// 这条盯的正是「歌词和播放条变了、列表没变」那个 bug。
    #[test]
    #[ignore]
    fn playing_mark_moves_with_current_track() {
        if !gtk::is_initialized() && gtk::init().is_err() {
            println!("跳过：没有可用的 display");
            return;
        }
        let boxes: Vec<gtk::Box> = (0..3)
            .map(|_| {
                let b = gtk::Box::new(gtk::Orientation::Horizontal, 10);
                b.add_css_class("track-row");
                b
            })
            .collect();
        let mut cells: HashMap<u32, gtk::Box> = HashMap::new();
        for (i, b) in boxes.iter().enumerate() {
            cells.insert(i as u32, b.clone());
        }
        let mut rows = vec![RowData::default(); 3];

        rows[0].playing = true;
        paint_cells(&cells, &rows);
        assert!(boxes[0].has_css_class("playing"));

        // 双击第 3 首：状态跟着 current 走
        rows[0].playing = false;
        rows[2].playing = true;
        paint_cells(&cells, &rows);
        let lit: Vec<usize> = boxes
            .iter()
            .enumerate()
            .filter(|(_, b)| b.has_css_class("playing"))
            .map(|(i, _)| i)
            .collect();
        assert_eq!(lit, vec![2], "只能有第 3 行是正在播放");
    }

    /// 主题必须按代码里实际挂类的节点（.track-row）来写选择器。
    /// 曾经写成 `.tracklist > row.playing`：列表的 row 节点拿不到，规则永不命中，
    /// 于是「正在播放」怎么切都不变。
    #[test]
    fn theme_targets_track_row_for_playing_state() {
        let css = theme::css("#3B82F6", true);
        assert!(css.contains(".track-row.playing .track-title"));
        assert!(css.contains(".track-row.playing {"));
        assert!(
            !css.contains("> row.playing"),
            "别把选择器写回列表的 row 节点，那里挂不上类"
        );
    }

    /// 当前行必须落在视口中线：内容坐标 - 滚动值 = 半屏。
    /// 这条挡住「按估算行高累加」那类漂移回归。
    #[test]
    fn lyric_line_stays_centered() {
        let view = 600.0;
        let margin = view / 2.0; // 上下各留半个视口
        for &(y, h) in &[(0.0, 30.0), (500.0, 44.0), (3000.0, 28.0), (12345.0, 52.0)] {
            let line_y = margin + y;
            let target = lyric_scroll_target(view, line_y, h, 0.0);
            let center_on_screen = line_y + h / 2.0 - target;
            assert!(
                (center_on_screen - view / 2.0).abs() < 0.001,
                "行 y={y} h={h} 偏离中线：{center_on_screen} vs {}",
                view / 2.0
            );
        }
    }

    /// 底栏内容区必须正好是 BAR_CONTENT_H：
    /// 左边封面 52、右边按钮 34 都在 64 内居中，中间 46+2+16 填满。
    /// 加 CSS 的上下 padding 12 就是 BAR_H=76。
    #[test]
    #[ignore]
    fn playerbar_content_height_is_fixed() {
        if !gtk::is_initialized() && gtk::init().is_err() {
            println!("跳过：没有可用的 display");
            return;
        }

        let bar = gtk::CenterBox::new();
        bar.add_css_class("playerbar");
        bar.set_size_request(-1, BAR_CONTENT_H);

        // 左：52px 封面（纹理已经是定尺的）
        let left = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        let pic = gtk::Picture::new();
        pic.set_content_fit(gtk::ContentFit::Cover);
        pic.set_size_request(BAR_COVER, BAR_COVER);
        let tex = {
            let b = glib::Bytes::from_owned(vec![0u8; (400 * 400 * 4) as usize]);
            let t: gdk::Texture =
                gdk::MemoryTexture::new(400, 400, gdk::MemoryFormat::R8g8b8a8, &b, 1600).upcast();
            t
        };
        let thumb = art::square(&tex, BAR_COVER as u32).unwrap();
        pic.set_paintable(Some(&thumb));
        left.append(&pic);

        // 中：按钮行 + 进度行
        let center = gtk::Box::new(gtk::Orientation::Vertical, 2);
        center.set_size_request(-1, BAR_CONTENT_H);
        let transport = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        transport.set_size_request(-1, 46);
        let prog = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        prog.set_size_request(-1, 16);
        center.append(&transport);
        center.append(&prog);

        bar.set_start_widget(Some(&left));
        bar.set_center_widget(Some(&center));

        let (min, nat, _, _) = bar.measure(gtk::Orientation::Vertical, -1);
        println!("playerbar 内容高度：min={min} natural={nat}，加上 CSS padding 即 {nat} + 12");
        assert_eq!(nat, BAR_CONTENT_H, "底栏内容区高度必须是常量");
        // 内容 + CSS 上下 padding(6+6) = BAR_H，两个常量必须自洽
        assert_eq!(nat + 12, BAR_H, "BAR_H 与 CSS padding 必须对得上");
    }
}
