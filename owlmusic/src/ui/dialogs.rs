//! 对话框：设置、歌词样式、在线歌词、在线元数据匹配。

use super::{art, spawn_task, App};
use crate::config::LyricStyle;
use crate::library;
use crate::lrc;
use crate::lyrics;
use crate::online::{self, Candidate};
use crate::tags::{self, Cover, MetaPatch};
use gtk::prelude::*;
use gtk::{gio, glib, pango};
use std::cell::RefCell;
use std::rc::Rc;

fn section(title: &str) -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 10);
    b.add_css_class("card");
    let l = gtk::Label::new(Some(title));
    l.set_xalign(0.0);
    l.add_css_class("sheet-title");
    b.append(&l);
    b
}

fn row(label: &str) -> (gtk::Box, gtk::Box) {
    let b = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let l = gtk::Label::new(Some(label));
    l.set_xalign(0.0);
    l.add_css_class("field-label");
    let sp = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    sp.set_hexpand(true);
    b.append(&l);
    b.append(&sp);
    let tail = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    b.append(&tail);
    (b, tail)
}

pub fn open_settings(app: Rc<App>) {
    let win = gtk::Window::builder()
        .title("设置")
        .transient_for(&app.window)
        .modal(true)
        .default_width(580)
        .default_height(700)
        .build();
    win.add_css_class("sheet");

    let root = gtk::Box::new(gtk::Orientation::Vertical, 14);
    root.set_margin_top(18);
    root.set_margin_bottom(18);
    root.set_margin_start(18);
    root.set_margin_end(18);

    // ---- 外观 ----
    let appearance = section("外观");
    let (r1, t1) = row("主题");
    let theme_dd = gtk::DropDown::from_strings(&["深色", "浅色"]);
    theme_dd.set_selected(if app.cfg.borrow().theme == "light" { 1 } else { 0 });
    t1.append(&theme_dd);
    appearance.append(&r1);

    let (r2, t2) = row("强调色跟随封面");
    let sw = gtk::Switch::new();
    sw.set_active(app.cfg.borrow().accent_follow_cover);
    sw.set_valign(gtk::Align::Center);
    t2.append(&sw);
    appearance.append(&r2);

    let (r3, t3) = row("强调色");
    let cd = gtk::ColorDialogButton::new(Some(gtk::ColorDialog::new()));
    let (ar, ag, ab) = art::parse_hex(&app.cfg.borrow().accent);
    cd.set_rgba(&gtk::gdk::RGBA::new(
        ar as f32 / 255.0,
        ag as f32 / 255.0,
        ab as f32 / 255.0,
        1.0,
    ));
    t3.append(&cd);
    let btn_follow = gtk::Button::with_label("跟随封面");
    btn_follow.add_css_class("ghost");
    t3.append(&btn_follow);
    appearance.append(&r3);

    {
        let a = app.clone();
        theme_dd.connect_selected_notify(move |dd| {
            let light = dd.selected() == 1;
            {
                let mut c = a.cfg.borrow_mut();
                c.theme = if light { "light".into() } else { "dark".into() };
                c.save();
            }
            if let Some(s) = gtk::Settings::default() {
                s.set_gtk_application_prefer_dark_theme(!light);
            }
            a.reload_css();
        });
    }
    {
        let a = app.clone();
        sw.connect_active_notify(move |s| {
            {
                let mut c = a.cfg.borrow_mut();
                c.accent_follow_cover = s.is_active();
                c.save();
            }
            if s.is_active() {
                // 先把 blur 拷出来，再进 if let：条件里的 `cfg.borrow()` 会活到
                // 整段块结束，块内 apply_accent() 要 borrow_mut，会 panic。
                let blur = a.cfg.borrow().blur;
                if let Some(t) = a.current_track() {
                    if let Some(c) = tags::read_cover(&t.path) {
                        if let Some(w) = art::from_bytes(&c.data, blur) {
                            a.apply_accent(&art::hex(w.accent));
                        }
                    }
                }
            }
        });
    }
    {
        let a = app.clone();
        cd.connect_rgba_notify(move |b| {
            let c = b.rgba();
            let hex = format!(
                "#{:02X}{:02X}{:02X}",
                (c.red() * 255.0) as u8,
                (c.green() * 255.0) as u8,
                (c.blue() * 255.0) as u8
            );
            {
                let mut cfg = a.cfg.borrow_mut();
                cfg.accent = hex.clone();
                cfg.accent_follow_cover = false;
                cfg.save();
            }
            a.reload_css();
        });
    }
    {
        let a = app.clone();
        let sf = sw.clone();
        btn_follow.connect_clicked(move |_| {
            // 开关自己会跑一遍「跟随封面」逻辑（从关到开会触发），
            // 只有「本来就开着」时按钮才需要自己动手，避免重复解码 + 重复重载样式表。
            let was_on = sf.is_active();
            sf.set_active(true);
            if !was_on {
                return;
            }
            let blur = a.cfg.borrow().blur;
            if let Some(idx) = a.current_index() {
                let path = a.tracks.borrow()[idx].path.clone();
                if let Some(c) = tags::read_cover(&path) {
                    if let Some(w) = art::from_bytes(&c.data, blur) {
                        a.apply_accent(&art::hex(w.accent));
                    }
                }
            }
        });
    }

    // ---- 播放与布局 ----
    let behavior = section("播放与布局");

    let (rb1, tb1) = row("歌单显示在右侧");
    let sw_side = gtk::Switch::new();
    sw_side.set_active(app.cfg.borrow().sidebar_right);
    sw_side.set_valign(gtk::Align::Center);
    tb1.append(&sw_side);
    behavior.append(&rb1);
    {
        let a = app.clone();
        sw_side.connect_active_notify(move |s| {
            {
                let mut c = a.cfg.borrow_mut();
                c.sidebar_right = s.is_active();
                c.save();
            }
            a.apply_layout();
        });
    }

    let (rb2, tb2) = row("歌词页使用封面背景");
    let sw_bg = gtk::Switch::new();
    sw_bg.set_active(app.cfg.borrow().cover_bg);
    sw_bg.set_valign(gtk::Align::Center);
    tb2.append(&sw_bg);
    behavior.append(&rb2);
    {
        let a = app.clone();
        sw_bg.connect_active_notify(move |s| {
            {
                let mut c = a.cfg.borrow_mut();
                c.cover_bg = s.is_active();
                c.save();
            }
            a.apply_cover_bg();
        });
    }

    let (rb3, tb3) = row("背景模糊强度");
    let blur_scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.2, 2.0, 0.05);
    blur_scale.set_draw_value(false);
    blur_scale.set_size_request(148, -1);
    blur_scale.set_value(app.cfg.borrow().blur as f64);
    blur_scale.set_valign(gtk::Align::Center);
    let blur_val = gtk::Label::new(Some(&format!("{:.2}", app.cfg.borrow().blur)));
    blur_val.add_css_class("value-label");
    blur_val.set_xalign(1.0);
    tb3.append(&blur_scale);
    tb3.append(&blur_val);
    behavior.append(&rb3);
    {
        let a = app.clone();
        let bv = blur_val.clone();
        blur_scale.connect_value_changed(move |s| {
            let v = s.value() as f32;
            bv.set_text(&format!("{v:.2}"));
            {
                let mut c = a.cfg.borrow_mut();
                c.blur = v;
                c.save();
            }
            let a2 = a.clone();
            glib::timeout_add_local_once(std::time::Duration::from_millis(280), move || {
                a2.refresh_blur()
            });
        });
    }

    let (rb3b, tb3b) = row("背景不透明度");
    let op_scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.05, 0.95, 0.01);
    op_scale.set_draw_value(false);
    op_scale.set_size_request(148, -1);
    op_scale.set_value(app.cfg.borrow().lyric_bg_opacity as f64);
    op_scale.set_valign(gtk::Align::Center);
    let op_val = gtk::Label::new(Some(&format!(
        "{:.0}%",
        app.cfg.borrow().lyric_bg_opacity * 100.0
    )));
    op_val.add_css_class("value-label");
    op_val.set_xalign(1.0);
    tb3b.append(&op_scale);
    tb3b.append(&op_val);
    behavior.append(&rb3b);
    {
        let a = app.clone();
        let ov = op_val.clone();
        op_scale.connect_value_changed(move |s| {
            let v = s.value() as f32;
            ov.set_text(&format!("{:.0}%", v * 100.0));
            {
                let mut c = a.cfg.borrow_mut();
                c.lyric_bg_opacity = v;
                c.save();
            }
            // 只改透明度，不用重新解码封面
            a.apply_bg_opacity();
        });
    }

    let (rb4, tb4) = row("启动时接着上次播放");
    let sw_resume = gtk::Switch::new();
    sw_resume.set_active(app.cfg.borrow().resume);
    sw_resume.set_valign(gtk::Align::Center);
    tb4.append(&sw_resume);
    behavior.append(&rb4);
    {
        let a = app.clone();
        sw_resume.connect_active_notify(move |s| {
            let mut c = a.cfg.borrow_mut();
            c.resume = s.is_active();
            c.save();
        });
    }

    // ---- 歌词 ----
    let lyric_card = section("歌词");

    let (lb0, tb0) = row("没有歌词时自动在线匹配");
    let sw_auto = gtk::Switch::new();
    sw_auto.set_active(app.cfg.borrow().auto_lyric);
    sw_auto.set_valign(gtk::Align::Center);
    tb0.append(&sw_auto);
    lyric_card.append(&lb0);
    {
        let a = app.clone();
        sw_auto.connect_active_notify(move |s| {
            let mut c = a.cfg.borrow_mut();
            c.auto_lyric = s.is_active();
            c.save();
        });
    }

    let (lb1, tb1) = row("歌词文件夹");
    let dir_label = gtk::Label::new(None);
    dir_label.set_xalign(0.0);
    dir_label.set_ellipsize(pango::EllipsizeMode::Middle);
    dir_label.set_margin_start(2);
    dir_label.add_css_class("hint");
    let clear_btn = gtk::Button::from_icon_name("edit-clear-symbolic");
    clear_btn.add_css_class("flat");
    clear_btn.set_tooltip_text(Some("不指定歌词文件夹"));
    let pick_btn = gtk::Button::from_icon_name("folder-open-symbolic");
    pick_btn.add_css_class("ghost");
    pick_btn.set_tooltip_text(Some("选择歌词文件夹"));
    tb1.append(&clear_btn);
    tb1.append(&pick_btn);
    lyric_card.append(&lb1);
    // 路径单独一行：文件名短的时候不会把按钮挤到边上去
    lyric_card.append(&dir_label);

    let (lb2, tb2) = row("如无歌词，自动下载到此文件夹");
    let sw_save = gtk::Switch::new();
    sw_save.set_active(app.cfg.borrow().lyric_auto_save);
    // 任何状态下都能点：选了文件夹之后当然也能关掉。
    // 没有文件夹时勾上不产生副作用，旁边那行说明会讲清楚。
    sw_save.set_valign(gtk::Align::Center);
    tb2.append(&sw_save);
    lyric_card.append(&lb2);

    let save_hint = gtk::Label::new(None);
    save_hint.set_xalign(0.0);
    save_hint.set_wrap(true);
    save_hint.add_css_class("hint");
    lyric_card.append(&save_hint);

    // 第二个开关：选中的歌词要不要顺手写进音频文件自己的歌词标签。
    // 和「下载到歌词文件夹」是两件事：那边是外部 .lrc，这边改的是音频文件本身，
    // 换了电脑/播放器也带着走。默认开，随时能关。
    let (lb3, tb3) = row("选择歌词后自动写入源文件");
    let sw_tag = gtk::Switch::new();
    sw_tag.set_active(app.cfg.borrow().lyric_auto_tag);
    sw_tag.set_valign(gtk::Align::Center);
    tb3.append(&sw_tag);
    lyric_card.append(&lb3);

    let tag_hint = gtk::Label::new(None);
    tag_hint.set_xalign(0.0);
    tag_hint.set_wrap(true);
    tag_hint.add_css_class("hint");
    lyric_card.append(&tag_hint);

    {
        let hint = tag_hint.clone();
        let sync_tag_hint = Rc::new(move |on: bool| {
            hint.set_text(if on {
                "在「选择歌词」里点「使用这份歌词」后，会把歌词写进当前这首歌的标签（ID3 的 USLT、Vorbis 的 LYRICS、MP4 的 ©lyr、APE 的 Lyrics）；只写这一首，不批量改。写入失败或该格式没有歌词字段时，状态栏会说明。"
            } else {
                "只显示歌词，不改音频文件里的标签。"
            });
        });
        (*sync_tag_hint)(sw_tag.is_active());
        let s = sync_tag_hint.clone();
        let a = app.clone();
        sw_tag.connect_active_notify(move |sw| {
            let on = sw.is_active();
            {
                let mut c = a.cfg.borrow_mut();
                c.lyric_auto_tag = on;
                c.save();
            }
            s(on);
        });
    }

    let lyric_hint = gtk::Label::new(Some(
        "找歌词的顺序：音频文件内嵌歌词 → 歌词文件夹 → 音频同目录的 .lrc → 在线检索。文件夹里认「歌手 - 曲名」「曲名」「01 曲名」这几种命名，不分大小写。",
    ));
    lyric_hint.set_xalign(0.0);
    lyric_hint.set_wrap(true);
    lyric_hint.add_css_class("hint");
    lyric_card.append(&lyric_hint);

    // 文件夹与开关的状态只在 cur_dir 里存一份，界面由 sync_ui 统一刷新，
    // 免得「选择/清除/切开关」三处各改一半、显示和配置对不上
    let cur_dir: Rc<RefCell<Option<std::path::PathBuf>>> =
        Rc::new(RefCell::new(app.cfg.borrow().lyric_dir.clone()));
    let sync_ui: Rc<dyn Fn()> = {
        let dl = dir_label.clone();
        let sh = save_hint.clone();
        let sw = sw_save.clone();
        let cur = cur_dir.clone();
        Rc::new(move || {
            let dir = cur.borrow().clone();
            match &dir {
                Some(p) => {
                    let name = p
                        .file_name()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_else(|| p.to_string_lossy().to_string());
                    dl.set_text(&name);
                    dl.set_tooltip_text(Some(&p.to_string_lossy()));
                }
                None => {
                    dl.set_text("未设置");
                    dl.set_tooltip_text(None);
                }
            }
            let text = match (&dir, sw.is_active()) {
                (None, _) => {
                    "还没选歌词文件夹，这个开关暂时不起作用；选好文件夹后才会往那里写。"
                }
                (Some(_), true) => {
                    "在线匹配到歌词后会存成「歌手 - 曲名.lrc」放进上面的文件夹，同名文件会被覆盖。"
                }
                (Some(_), false) => {
                    "只显示、不写文件。程序自己的缓存仍会保留一份，下次离线也能看。"
                }
            };
            sh.set_text(text);
        })
    };
    sync_ui();

    {
        let a = app.clone();
        let sync = sync_ui.clone();
        sw_save.connect_active_notify(move |s| {
            {
                let mut c = a.cfg.borrow_mut();
                c.lyric_auto_save = s.is_active();
                c.save();
            }
            sync();
        });
    }
    {
        let a = app.clone();
        let w = win.clone();
        let sw = sw_save.clone();
        let cur = cur_dir.clone();
        let sync = sync_ui.clone();
        pick_btn.connect_clicked(move |_| {
            let dlg = gtk::FileDialog::builder()
                .title("选择歌词文件夹")
                .modal(true)
                .build();
            let a2 = a.clone();
            let sw2 = sw.clone();
            let cur2 = cur.clone();
            let sync2 = sync.clone();
            dlg.select_folder(Some(&w), None::<&gio::Cancellable>, move |res| {
                let Ok(f) = res else { return };
                let Some(p) = f.path() else { return };
                // 第一次选目录才默认勾上「自动下载」；之后用户自己关掉是算数的，
                // 换个目录不会又把开关顶回打开
                let first_time = cur2.borrow().is_none();
                {
                    let mut c = a2.cfg.borrow_mut();
                    c.lyric_dir = Some(p.clone());
                    if first_time {
                        c.lyric_auto_save = true;
                    }
                    c.save();
                }
                *cur2.borrow_mut() = Some(p);
                if first_time {
                    sw2.set_active(true);
                }
                sync2();
                a2.set_status(if sw2.is_active() {
                    "歌词文件夹已设置：先来这里找，没有就下载到这里"
                } else {
                    "歌词文件夹已设置；自动下载是关的，歌词只显示不落盘"
                });
                // 当前这首歌可能一直显示「暂无歌词」，立刻按新位置重找一遍
                if let Some(t) = a2.current_track() {
                    a2.load_lyrics(&t);
                }
            });
        });
    }
    {
        let a = app.clone();
        let cur = cur_dir.clone();
        let sync = sync_ui.clone();
        clear_btn.connect_clicked(move |_| {
            {
                let mut c = a.cfg.borrow_mut();
                c.lyric_dir = None;
                c.save();
            }
            *cur.borrow_mut() = None;
            sync();
            a.set_status("已取消歌词文件夹，改回从音频同目录找 .lrc");
        });
    }

    // ---- 在线 ----
    let online_card = section("在线元数据");
    let (r4, t4) = row("搜索区域");
    let country_dd = gtk::DropDown::from_strings(&["日本 jp", "中国 cn", "美国 us", "台湾 tw"]);
    let keys = ["jp", "cn", "us", "tw"];
    let cur_country = app.cfg.borrow().country.clone();
    country_dd.set_selected(keys.iter().position(|k| *k == cur_country).unwrap_or(0) as u32);
    t4.append(&country_dd);
    online_card.append(&r4);
    {
        let a = app.clone();
        country_dd.connect_selected_notify(move |dd| {
            let i = dd.selected() as usize;
            if let Some(k) = keys.get(i) {
                let mut c = a.cfg.borrow_mut();
                c.country = (*k).to_string();
                c.save();
            }
        });
    }
    let hint = gtk::Label::new(Some(
        "标题/艺人为空时会用文件名推断，ACG 常见的「[社团] 曲名」「01. 曲名」都能识别。",
    ));
    hint.set_xalign(0.0);
    hint.set_wrap(true);
    hint.add_css_class("hint");
    online_card.append(&hint);

    // ---- 曲库 ----
    let lib_card = section("音乐文件夹");
    let folder_list = gtk::Box::new(gtk::Orientation::Vertical, 6);
    let rebuild_folders = {
        let app = app.clone();
        let folder_list = folder_list.clone();
        Rc::new(move || {
            while let Some(c) = folder_list.first_child() {
                folder_list.remove(&c);
            }
            let roots = app.cfg.borrow().roots.clone();
            if roots.is_empty() {
                let l = gtk::Label::new(Some("还没有添加文件夹"));
                l.set_xalign(0.0);
                l.add_css_class("hint");
                folder_list.append(&l);
            }
            for (i, p) in roots.iter().enumerate() {
                let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
                let l = gtk::Label::new(Some(&p.to_string_lossy()));
                l.set_xalign(0.0);
                l.set_ellipsize(pango::EllipsizeMode::Middle);
                l.set_max_width_chars(38);
                l.set_hexpand(true);
                l.add_css_class("field-label");
                let del = gtk::Button::from_icon_name("list-remove-symbolic");
                del.add_css_class("flat");
                del.set_tooltip_text(Some("移除"));
                {
                    let app = app.clone();
                    del.connect_clicked(move |_| {
                        {
                            let mut c = app.cfg.borrow_mut();
                            if i < c.roots.len() {
                                c.roots.remove(i);
                                c.save();
                            }
                        }
                        app.set_status("已移除文件夹，点刷新重新扫描");
                    });
                }
                line.append(&l);
                line.append(&del);
                folder_list.append(&line);
            }
        })
    };
    rebuild_folders();
    lib_card.append(&folder_list);

    let lib_btns = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let btn_add = gtk::Button::with_label("添加文件夹");
    btn_add.add_css_class("ghost");
    let btn_scan = gtk::Button::with_label("重新扫描");
    btn_scan.add_css_class("accent");
    lib_btns.append(&btn_add);
    lib_btns.append(&btn_scan);
    lib_card.append(&lib_btns);
    {
        let a = app.clone();
        let w = win.clone();
        let rf = rebuild_folders.clone();
        btn_add.connect_clicked(move |_| {
            let dlg = gtk::FileDialog::builder()
                .title("选择音乐文件夹")
                .modal(true)
                .build();
            let a2 = a.clone();
            let rf2 = rf.clone();
            dlg.select_folder(Some(&w), None::<&gio::Cancellable>, move |res| {
                if let Ok(f) = res {
                    if let Some(p) = f.path() {
                        {
                            let mut c = a2.cfg.borrow_mut();
                            if !c.roots.contains(&p) {
                                c.roots.push(p);
                                c.save();
                            }
                        }
                        rf2();
                        a2.rescan();
                    }
                }
            });
        });
    }
    {
        let a = app.clone();
        btn_scan.connect_clicked(move |_| {
            a.rescan();
        });
    }

    // ---- 关于 ----
    let about = section("关于");
    let line = gtk::Label::new(Some(
        "owlmusic 0.1 · GTK4 + Rust · symphonia 解码 / ALSA 输出 · 元数据 iTunes + MusicBrainz · 歌词 LRCLIB\n快捷键：空格 播放暂停 · ← → 前后 5 秒 · 点击歌词行跳转",
    ));
    line.set_xalign(0.0);
    line.set_wrap(true);
    line.add_css_class("hint");
    about.append(&line);

    root.append(&appearance);
    root.append(&behavior);
    root.append(&lyric_card);
    root.append(&online_card);
    root.append(&lib_card);
    root.append(&about);

    let scroll = gtk::ScrolledWindow::new();
    scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scroll.set_child(Some(&root));
    win.set_child(Some(&scroll));
    win.present();
}

pub fn open_lyric_style(app: Rc<App>, parent: &impl IsA<gtk::Widget>) {
    let pop = gtk::Popover::new();
    pop.add_css_class("owl-pop");
    pop.set_parent(parent);
    pop.set_has_arrow(true);

    let root = gtk::Box::new(gtk::Orientation::Vertical, 10);
    root.set_size_request(320, -1);

    let title = gtk::Label::new(Some("歌词样式"));
    title.set_xalign(0.0);
    title.add_css_class("sheet-title");
    root.append(&title);

    // 字号
    let (r1, t1) = row("字号");
    let minus = gtk::Button::from_icon_name("zoom-out-symbolic");
    minus.add_css_class("flat");
    let val = gtk::Label::new(Some(&app.cfg.borrow().lyric.font_size.to_string()));
    val.add_css_class("value-label");
    let plus = gtk::Button::from_icon_name("zoom-in-symbolic");
    plus.add_css_class("flat");
    t1.append(&minus);
    t1.append(&val);
    t1.append(&plus);
    root.append(&r1);

    // 颜色
    let (r2, t2) = row("颜色");
    let cd = gtk::ColorDialogButton::new(Some(gtk::ColorDialog::new()));
    let (cr, cg, cb) = app.lyric_color_pub();
    cd.set_rgba(&gtk::gdk::RGBA::new(
        cr as f32 / 255.0,
        cg as f32 / 255.0,
        cb as f32 / 255.0,
        1.0,
    ));
    let b_theme = gtk::Button::with_label("主题");
    b_theme.add_css_class("ghost");
    let b_accent = gtk::Button::with_label("强调");
    b_accent.add_css_class("ghost");
    t2.append(&cd);
    t2.append(&b_theme);
    t2.append(&b_accent);
    root.append(&r2);

    // 对齐
    let (r3, t3) = row("对齐");
    let align = app.cfg.borrow().lyric.align;
    let b_left = gtk::ToggleButton::with_label("左");
    let b_center = gtk::ToggleButton::with_label("中");
    let b_right = gtk::ToggleButton::with_label("右");
    for b in [&b_left, &b_center, &b_right] {
        b.add_css_class("ghost");
    }
    b_left.set_active(align == 0);
    b_center.set_active(align == 1);
    b_right.set_active(align == 2);
    t3.append(&b_left);
    t3.append(&b_center);
    t3.append(&b_right);
    root.append(&r3);

    // 垂直位置
    let (r4, t4) = row("垂直位置");
    let offset = gtk::Scale::with_range(gtk::Orientation::Horizontal, -260.0, 260.0, 4.0);
    offset.set_value(app.cfg.borrow().lyric.offset_px as f64);
    offset.set_size_request(150, -1);
    offset.set_draw_value(false);
    t4.append(&offset);
    root.append(&r4);

    // 行距
    let (r5, t5) = row("行距");
    let spacing = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 40.0, 1.0);
    spacing.set_value(app.cfg.borrow().lyric.line_spacing as f64);
    spacing.set_size_request(150, -1);
    spacing.set_draw_value(false);
    t5.append(&spacing);
    root.append(&r5);

    // 其余行淡出
    let (r6, t6) = row("其它行透明度");
    let dim = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.12, 0.9, 0.01);
    dim.set_value(app.cfg.borrow().lyric.dim as f64);
    dim.set_size_request(150, -1);
    dim.set_draw_value(false);
    t6.append(&dim);
    root.append(&r6);

    // 翻译 / 空行
    let (r7, t7) = row("显示翻译行");
    let sw_tr = gtk::Switch::new();
    sw_tr.set_active(app.cfg.borrow().lyric.show_trans);
    sw_tr.set_valign(gtk::Align::Center);
    t7.append(&sw_tr);
    root.append(&r7);

    let (r8, t8) = row("隐藏空行");
    let sw_empty = gtk::Switch::new();
    sw_empty.set_active(app.cfg.borrow().lyric.hide_empty);
    sw_empty.set_valign(gtk::Align::Center);
    t8.append(&sw_empty);
    root.append(&r8);

    // ---- 事件 ----
    {
        let a = app.clone();
        let v = val.clone();
        plus.connect_clicked(move |_| {
            let mut c = a.cfg.borrow_mut();
            c.lyric.font_size = (c.lyric.font_size + 2).min(48);
            let f = c.lyric.font_size;
            c.save();
            v.set_text(&f.to_string());
            drop(c);
            a.restyle_lyrics();
        });
    }
    {
        let a = app.clone();
        let v = val.clone();
        minus.connect_clicked(move |_| {
            let mut c = a.cfg.borrow_mut();
            c.lyric.font_size = (c.lyric.font_size - 2).max(12);
            let f = c.lyric.font_size;
            c.save();
            v.set_text(&f.to_string());
            drop(c);
            a.restyle_lyrics();
        });
    }
    {
        let a = app.clone();
        cd.connect_rgba_notify(move |b| {
            let c = b.rgba();
            let hex = format!(
                "#{:02X}{:02X}{:02X}",
                (c.red() * 255.0) as u8,
                (c.green() * 255.0) as u8,
                (c.blue() * 255.0) as u8
            );
            {
                let mut cfg = a.cfg.borrow_mut();
                cfg.lyric.color = hex;
                cfg.save();
            }
            a.restyle_lyrics();
        });
    }
    {
        let a = app.clone();
        b_theme.connect_clicked(move |_| {
            a.cfg.borrow_mut().lyric.color = "text".into();
            a.cfg.borrow().save();
            a.restyle_lyrics();
        });
    }
    {
        let a = app.clone();
        b_accent.connect_clicked(move |_| {
            a.cfg.borrow_mut().lyric.color = "accent".into();
            a.cfg.borrow().save();
            a.restyle_lyrics();
        });
    }
    for (btn, mode) in [(&b_left, 0u8), (&b_center, 1), (&b_right, 2)] {
        let a = app.clone();
        let bl = b_left.clone();
        let bc = b_center.clone();
        let br = b_right.clone();
        btn.connect_clicked(move |_| {
            {
                let mut c = a.cfg.borrow_mut();
                c.lyric.align = mode;
                c.save();
            }
            bl.set_active(mode == 0);
            bc.set_active(mode == 1);
            br.set_active(mode == 2);
            a.restyle_lyrics();
        });
    }
    {
        let a = app.clone();
        offset.connect_change_value(move |_, _, v| {
            let mut c = a.cfg.borrow_mut();
            c.lyric.offset_px = v as i32;
            c.save();
            glib::Propagation::Proceed
        });
        let a2 = app.clone();
        offset.connect_value_changed(move |_| {
            a2.refresh_lyric_offset();
        });
    }
    {
        let a = app.clone();
        spacing.connect_value_changed(move |s| {
            let mut c = a.cfg.borrow_mut();
            c.lyric.line_spacing = s.value() as i32;
            c.save();
            drop(c);
            a.restyle_lyrics();
        });
    }
    {
        let a = app.clone();
        dim.connect_value_changed(move |s| {
            let mut c = a.cfg.borrow_mut();
            c.lyric.dim = s.value() as f32;
            c.save();
            drop(c);
            a.restyle_lyrics();
        });
    }
    {
        let a = app.clone();
        sw_tr.connect_active_notify(move |s| {
            let mut c = a.cfg.borrow_mut();
            c.lyric.show_trans = s.is_active();
            c.save();
            drop(c);
            a.restyle_lyrics();
        });
    }
    {
        let a = app.clone();
        sw_empty.connect_active_notify(move |s| {
            let mut c = a.cfg.borrow_mut();
            c.lyric.hide_empty = s.is_active();
            c.save();
            drop(c);
            a.restyle_lyrics();
        });
    }

    pop.set_child(Some(&root));
    pop.popup();
}

/// 手动拿到的歌词落盘：设了歌词文件夹就存那儿（不污染音乐目录），
/// 否则仍放音频旁边。返回最终写到了哪个文件。
fn save_lyric_file(
    app: &App,
    track: &library::Track,
    text: &str,
) -> Option<std::path::PathBuf> {
    let dir = app.cfg.borrow().lyric_dir.clone();
    match dir {
        Some(d) => lyrics::save(&d, track, text).ok(),
        None => {
            let side = track.path.with_extension("lrc");
            std::fs::write(&side, text).ok().map(|_| side)
        }
    }
}

pub fn fetch_online_lyric(app: Rc<App>) {
    open_lyric_picker(app);
}

/// 在线搜歌词：一次列出所有候选，让人自己挑（以前是闷头取第一条）。
pub fn open_lyric_picker(app: Rc<App>) {
    let Some(track) = app.current_track() else {
        app.set_status("先选一首歌再搜索歌词");
        return;
    };

    let win = gtk::Window::builder()
        .title("选择歌词")
        .transient_for(&app.window)
        .modal(true)
        .default_width(720)
        .default_height(660)
        .build();
    win.add_css_class("sheet");

    let root = gtk::Box::new(gtk::Orientation::Vertical, 10);
    root.set_margin_top(16);
    root.set_margin_bottom(16);
    root.set_margin_start(16);
    root.set_margin_end(16);

    let term = gtk::Entry::new();
    let preset = if track.artist.trim().is_empty() {
        track.title.clone()
    } else {
        format!("{} {}", track.artist, track.title)
    };
    term.set_text(&preset);
    term.add_css_class("owl-search");
    term.set_hexpand(true);
    let btn_search = gtk::Button::with_label("搜索");
    btn_search.add_css_class("accent");
    let top = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    top.append(&term);
    top.append(&btn_search);
    root.append(&top);

    let cur_line = gtk::Label::new(Some(&format!(
        "当前：{} — {}{}",
        track.title,
        track.display_artist(),
        if track.dur_ms > 0 {
            format!("（{}）", library::fmt_time(track.dur_ms))
        } else {
            String::new()
        }
    )));
    cur_line.set_xalign(0.0);
    cur_line.add_css_class("hint");
    root.append(&cur_line);

    let list = gtk::ListBox::new();
    list.add_css_class("matchlist");
    list.set_selection_mode(gtk::SelectionMode::Single);
    let list_scroll = gtk::ScrolledWindow::new();
    list_scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    list_scroll.set_child(Some(&list));
    list_scroll.set_vexpand(true);
    list_scroll.set_min_content_height(220);
    root.append(&list_scroll);

    let preview = gtk::Label::new(Some("选中一条结果后这里显示歌词开头"));
    preview.set_xalign(0.0);
    preview.set_yalign(0.0);
    preview.set_wrap(true);
    preview.set_wrap_mode(pango::WrapMode::WordChar);
    preview.set_selectable(true);
    preview.set_margin_top(8);
    preview.set_margin_bottom(8);
    preview.set_margin_start(10);
    preview.set_margin_end(10);
    preview.add_css_class("lyric-peek");
    let preview_scroll = gtk::ScrolledWindow::new();
    preview_scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    preview_scroll.set_child(Some(&preview));
    preview_scroll.set_min_content_height(150);
    preview_scroll.add_css_class("peek-box");
    root.append(&preview_scroll);

    let status = gtk::Label::new(Some("正在检索歌词…"));
    status.set_xalign(0.0);
    status.set_wrap(true);
    status.add_css_class("hint");
    root.append(&status);

    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    actions.set_halign(gtk::Align::End);
    let btn_cancel = gtk::Button::with_label("取消");
    btn_cancel.add_css_class("ghost");
    let btn_apply = gtk::Button::with_label("使用这份歌词");
    btn_apply.add_css_class("accent");
    btn_apply.set_sensitive(false);
    actions.append(&btn_cancel);
    actions.append(&btn_apply);
    root.append(&actions);

    win.set_child(Some(&root));
    win.present();

    let cands: Rc<RefCell<Vec<online::LyricCandidate>>> = Rc::new(RefCell::new(Vec::new()));
    let chosen: Rc<RefCell<Option<usize>>> = Rc::new(RefCell::new(None));

    // ---- 搜索（None = 用当前曲目的标签，Some = 用户自己敲的词）----
    let run: Rc<dyn Fn(Option<String>)> = {
        let list = list.clone();
        let cands = cands.clone();
        let chosen = chosen.clone();
        let status = status.clone();
        let preview = preview.clone();
        let btn_apply = btn_apply.clone();
        let track = track.clone();
        Rc::new(move |q: Option<String>| {
            status.set_text("正在检索歌词…");
            let artist = track.artist.clone();
            let title = track.title.clone();
            let album = track.album.clone();
            let target = (track.dur_ms > 0).then_some(track.dur_ms);
            let list2 = list.clone();
            let cands2 = cands.clone();
            let chosen2 = chosen.clone();
            let status2 = status.clone();
            let preview2 = preview.clone();
            let apply2 = btn_apply.clone();
            spawn_task(
                move || match q {
                    Some(t) => online::search_lyrics_text(&t, target),
                    None => online::search_lyrics(&artist, &title, &album, target),
                },
                move |res| {
                    while let Some(c) = list2.first_child() {
                        list2.remove(&c);
                    }
                    *chosen2.borrow_mut() = None;
                    apply2.set_sensitive(false);
                    preview2.set_text("选中一条结果后这里显示歌词开头");
                    match res {
                        Ok(items) => {
                            *cands2.borrow_mut() = items.clone();
                            for (i, c) in items.iter().enumerate() {
                                let rb = gtk::Box::new(gtk::Orientation::Horizontal, 10);
                                rb.add_css_class("match-row");
                                let col = gtk::Box::new(gtk::Orientation::Vertical, 2);
                                col.set_hexpand(true);
                                let t = gtk::Label::new(Some(if c.title.is_empty() {
                                    "(无标题)"
                                } else {
                                    &c.title
                                }));
                                t.set_xalign(0.0);
                                t.set_ellipsize(pango::EllipsizeMode::End);
                                t.add_css_class("match-title");
                                let mut sub = format!("{}", if c.artist.is_empty() { "-" } else { &c.artist });
                                if !c.album.is_empty() {
                                    sub.push_str(" · ");
                                    sub.push_str(&c.album);
                                }
                                if let Some(d) = c.dur_ms {
                                    sub.push_str(&format!(" · {}", library::fmt_time(d)));
                                }
                                sub.push_str(&format!(" · {} 行", c.line_count()));
                                let s = gtk::Label::new(Some(&sub));
                                s.set_xalign(0.0);
                                s.set_ellipsize(pango::EllipsizeMode::End);
                                s.add_css_class("match-sub");
                                col.append(&t);
                                col.append(&s);
                                let tag = gtk::Label::new(Some(if c.synced { "逐行" } else { "纯文本" }));
                                tag.add_css_class("badge");
                                tag.set_valign(gtk::Align::Center);
                                if c.synced {
                                    tag.add_css_class("badge-sync");
                                }
                                rb.append(&col);
                                rb.append(&tag);
                                let row = gtk::ListBoxRow::new();
                                row.set_child(Some(&rb));
                                row.set_widget_name(&i.to_string());
                                list2.append(&row);
                            }
                            status2.set_text(&format!(
                                "找到 {} 条，选一条看看，再点「使用这份歌词」",
                                items.len()
                            ));
                            // 默认选中最合适的那条（列表已按「逐行 + 时长接近」排好）
                            if let Some(first) = list2.row_at_index(0) {
                                list2.select_row(Some(&first));
                            }
                        }
                        Err(e) => status2.set_text(&format!("没搜到：{e}（可以改关键词再试）")),
                    }
                },
            );
        })
    };

    // ---- 选中：更新预览 ----
    {
        let cands = cands.clone();
        let chosen = chosen.clone();
        let preview = preview.clone();
        let status = status.clone();
        let btn_apply = btn_apply.clone();
        let track = track.clone();
        list.connect_row_selected(move |_, row| {
            let Some(row) = row else {
                *chosen.borrow_mut() = None;
                btn_apply.set_sensitive(false);
                return;
            };
            let Ok(i) = row.widget_name().parse::<usize>() else {
                return;
            };
            *chosen.borrow_mut() = Some(i);
            let cand = cands.borrow().get(i).cloned();
            let Some(c) = cand else { return };
            btn_apply.set_sensitive(true);
            let body = c.preview(18);
            let more = c.line_count().saturating_sub(18);
            preview.set_text(&if more > 0 {
                format!("{body}\n…（共 {} 行）", c.line_count())
            } else {
                body
            });
            let mut note = format!(
                "{} · {} 行",
                if c.synced { "逐行歌词，能跟着唱" } else { "纯文本，没有时间轴，只能静态显示" },
                c.line_count()
            );
            match c.dur_gap(Some(track.dur_ms)).filter(|_| track.dur_ms > 0) {
                Some(g) if g <= 2000 => note.push_str("，时长与本曲一致"),
                Some(g) => note.push_str(&format!("，比本曲差 {} 秒，可能对不上", g / 1000)),
                None => {}
            }
            status.set_text(&note);
        });
    }

    // ---- 使用这份歌词 ----
    let apply: Rc<dyn Fn()> = {
        let app = app.clone();
        let cands = cands.clone();
        let chosen = chosen.clone();
        let track = track.clone();
        let win = win.clone();
        Rc::new(move || {
            let Some(i) = *chosen.borrow() else {
                return;
            };
            let cand = cands.borrow().get(i).cloned();
            let Some(c) = cand else { return };
            let lyr = lrc::parse(&c.lrc);
            if lyr.is_empty() {
                app.set_status("这条歌词内容为空，换一条试试");
                return;
            }
            let n = lyr.lines.len();
            let synced = lyr.synced;
            app.set_lyrics(lyr);
            // 手点搜索就是要歌词：落盘规则和以前一样
            let saved = save_lyric_file(&app, &track, &c.lrc);
            let where_ = saved
                .as_ref()
                .and_then(|p| p.file_name())
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "未保存".into());
            // 「选择歌词后自动写入源文件」开着就把这份歌词写进音频文件自己的标签：
            // 和写 .lrc 是两条独立的路，一个改外部文件、一个改音频文件本身
            let auto_tag = { app.cfg.borrow().lyric_auto_tag };
            let tag_note = if auto_tag {
                match tags::write_lyrics(&track.path, &c.lrc) {
                    Ok(()) => {
                        // 标签变了，重读这条曲目（列表里的「有歌词」标记也跟着更新）。
                        // 歌词区不用重载，就是刚选中的这份。
                        if let Some(idx) = app.current_index() {
                            app.refresh_track(idx);
                        }
                        crate::guard::log(&format!(
                            "选中的歌词已写入源文件标签：{}",
                            track.path.display()
                        ));
                        Some("已写进源文件标签".to_string())
                    }
                    Err(e) => {
                        // 校验失败时错误信息是多行的，状态栏只放第一行
                        let first = e.lines().next().unwrap_or("未知原因");
                        Some(format!("写源文件标签失败：{first}"))
                    }
                }
            } else {
                None
            };
            let mut note = format!(
                "已应用{}歌词（{} 行，来源 {}）→ {}",
                if synced { "逐行" } else { "纯文本" },
                n,
                c.source,
                where_
            );
            if let Some(t) = tag_note {
                note.push_str("；");
                note.push_str(&t);
            }
            app.set_status(&note);
            win.close();
        })
    };

    {
        let r = run.clone();
        let t = term.clone();
        btn_search.connect_clicked(move |_| r(Some(t.text().to_string())));
    }
    {
        let r = run.clone();
        let t = term.clone();
        let t2 = term.clone();
        t.connect_activate(move |_| r(Some(t2.text().to_string())));
    }
    {
        let ap = apply.clone();
        btn_apply.connect_clicked(move |_| ap());
    }
    {
        // 双击直接应用，省一次点按钮
        let ap = apply.clone();
        list.connect_row_activated(move |_, _| ap());
    }
    {
        let w = win.clone();
        btn_cancel.connect_clicked(move |_| w.close());
    }

    // 打开就按当前标签搜一次
    run(None);
}

pub fn open_match(app: Rc<App>) {
    let blur_strength = app.cfg.borrow().blur;
    let Some(track) = app.current_track() else {
        app.set_status("先选一首歌再做在线匹配");
        return;
    };
    let win = gtk::Window::builder()
        .title("在线匹配元数据")
        .transient_for(&app.window)
        .modal(true)
        .default_width(680)
        .default_height(620)
        .build();
    win.add_css_class("sheet");

    let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
    root.set_margin_top(16);
    root.set_margin_bottom(16);
    root.set_margin_start(16);
    root.set_margin_end(16);

    let term = gtk::Entry::new();
    let preset = if track.artist.trim().is_empty() {
        track.title.clone()
    } else {
        format!("{} {}", track.artist, track.title)
    };
    term.set_text(&preset);
    term.add_css_class("owl-search");
    term.set_hexpand(true);
    let btn_search = gtk::Button::with_label("搜索");
    btn_search.add_css_class("accent");
    let top = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    top.append(&term);
    top.append(&btn_search);
    root.append(&top);

    let cur_line = gtk::Label::new(Some(&format!(
        "当前：{} — {}",
        track.title,
        track.display_artist()
    )));
    cur_line.set_xalign(0.0);
    cur_line.add_css_class("hint");
    root.append(&cur_line);

    let list = gtk::ListBox::new();
    list.add_css_class("matchlist");
    list.set_selection_mode(gtk::SelectionMode::Single);
    let list_scroll = gtk::ScrolledWindow::new();
    list_scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    list_scroll.set_child(Some(&list));
    list_scroll.set_vexpand(true);
    list_scroll.set_min_content_height(240);
    root.append(&list_scroll);

    let preview = gtk::Picture::new();
    preview.set_content_fit(gtk::ContentFit::Cover);
    preview.set_size_request(96, 96);
    let preview_frame = gtk::Box::new(gtk::Orientation::Vertical, 0);
    preview_frame.add_css_class("cover-frame");
    preview_frame.set_size_request(96, 96);
    preview_frame.set_valign(gtk::Align::Center);
    preview_frame.append(&preview);

    let status = gtk::Label::new(Some(
        "输入关键词后点搜索，选一条结果，再点「编辑并保存」带到标签编辑页",
    ));
    status.set_xalign(0.0);
    status.set_wrap(true);
    status.set_hexpand(true);
    status.add_css_class("hint");

    let bottom = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    bottom.append(&preview_frame);
    bottom.append(&status);
    root.append(&bottom);

    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    actions.set_halign(gtk::Align::End);
    let btn_cancel = gtk::Button::with_label("取消");
    btn_cancel.add_css_class("ghost");
    let btn_cover = gtk::Button::with_label("仅下载封面");
    btn_cover.add_css_class("ghost");
    let btn_apply = gtk::Button::with_label("编辑并保存…");
    btn_apply.add_css_class("accent");
    actions.append(&btn_cancel);
    actions.append(&btn_cover);
    actions.append(&btn_apply);
    root.append(&actions);

    win.set_child(Some(&root));
    win.present();

    let cands: Rc<RefCell<Vec<Candidate>>> = Rc::new(RefCell::new(Vec::new()));
    let chosen: Rc<RefCell<Option<usize>>> = Rc::new(RefCell::new(None));
    let cover_bytes: Rc<RefCell<Option<Vec<u8>>>> = Rc::new(RefCell::new(None));

    // 搜索
    let run_search: Rc<dyn Fn()> = {
        let app = app.clone();
        let list = list.clone();
        let cands = cands.clone();
        let chosen = chosen.clone();
        let preview = preview.clone();
        let status = status.clone();
        let term = term.clone();
        Rc::new(move || {
            let q = term.text().to_string();
            if q.trim().is_empty() {
                status.set_text("请输入搜索关键词");
                return;
            }
            status.set_text("正在搜索…");
            let country = app.cfg.borrow().country.clone();
            let list2 = list.clone();
            let cands2 = cands.clone();
            let chosen2 = chosen.clone();
            let preview2 = preview.clone();
            let status2 = status.clone();
            spawn_task(
                move || online::search_all(&q, &country),
                move |res| match res {
                    Ok(items) => {
                        while let Some(c) = list2.first_child() {
                            list2.remove(&c);
                        }
                        *cands2.borrow_mut() = items.clone();
                        *chosen2.borrow_mut() = None;
                        preview2.set_paintable(None::<&gtk::gdk::Texture>);
                        status2.set_text(&format!("找到 {} 条结果", items.len()));
                        for (i, c) in items.iter().enumerate() {
                            let rb = gtk::Box::new(gtk::Orientation::Horizontal, 10);
                            rb.add_css_class("match-row");
                            let col = gtk::Box::new(gtk::Orientation::Vertical, 2);
                            col.set_hexpand(true);
                            let title_text = if c.title.is_empty() { "(无标题)".to_string() } else { c.title.clone() };
                            let t = gtk::Label::new(Some(&title_text));
                            t.set_xalign(0.0);
                            t.set_ellipsize(pango::EllipsizeMode::End);
                            t.add_css_class("match-title");
                            let sub = format!(
                                "{} · {} · {}",
                                c.artist,
                                if c.album.is_empty() { "-" } else { &c.album },
                                c.year.map(|y| y.to_string()).unwrap_or_else(|| "-".into())
                            );
                            let s = gtk::Label::new(Some(&sub));
                            s.set_xalign(0.0);
                            s.set_ellipsize(pango::EllipsizeMode::End);
                            s.add_css_class("match-sub");
                            col.append(&t);
                            col.append(&s);
                            let src = gtk::Label::new(Some(&c.source));
                            src.add_css_class("badge");
                            rb.append(&col);
                            rb.append(&src);
                            let row = gtk::ListBoxRow::new();
                            row.set_child(Some(&rb));
                            row.set_widget_name(&i.to_string());
                            list2.append(&row);
                        }
                    }
                    Err(e) => status2.set_text(&format!("搜索失败：{e}")),
                },
            );
        })
    };
    {
        let rs = run_search.clone();
        btn_search.connect_clicked(move |_| rs());
    }
    {
        let rs = run_search.clone();
        term.connect_activate(move |_| rs());
    }

    // 选中 → 拉封面预览
    {
        let list = list.clone();
        let cands = cands.clone();
        let chosen = chosen.clone();
        let cover_bytes = cover_bytes.clone();
        let preview = preview.clone();
        let status = status.clone();
        list.connect_row_selected(move |_, row| {
            let Some(row) = row else { return };
            let Ok(i) = row.widget_name().parse::<usize>() else {
                return;
            };
            *chosen.borrow_mut() = Some(i);
            let cand = cands.borrow().get(i).cloned();
            let Some(c) = cand else { return };
            if let Some(url) = c.cover_url.clone() {
                status.set_text("正在下载封面预览…");
                let cover_bytes = cover_bytes.clone();
                let preview = preview.clone();
                let status = status.clone();
                let blur = blur_strength;
                spawn_task(
                    move || online::fetch(&url),
                    move |res| match res {
                        Ok(bytes) => {
                            if let Some(w) = art::from_bytes(&bytes, blur) {
                                // 必须是定尺缩略图，否则原图会把对话框撑大
                                let thumb = art::square(&w.cover, 96);
                                preview.set_paintable(thumb.as_ref());
                            }
                            *cover_bytes.borrow_mut() = Some(bytes);
                            status.set_text("封面已就绪，可以应用");
                        }
                        Err(e) => status.set_text(&format!("封面下载失败：{e}")),
                    },
                );
            } else {
                status.set_text("这条结果没有封面");
            }
        });
    }

    // 取消
    {
        let w = win.clone();
        btn_cancel.connect_clicked(move |_| {
            w.close();
        });
    }

    // 仅下载封面
    {
        let a = app.clone();
        let cands = cands.clone();
        let chosen = chosen.clone();
        let cover_bytes = cover_bytes.clone();
        let status = status.clone();
        let path = track.path.clone();
        btn_cover.connect_clicked(move |_| {
            let Some(i) = *chosen.borrow() else {
                status.set_text("先选一条结果");
                return;
            };
            let cand = cands.borrow().get(i).cloned();
            let Some(c) = cand else { return };
            let cached = cover_bytes.borrow().clone();
            let a2 = a.clone();
            let status2 = status.clone();
            let path_main = path.clone();
            let finish = move |bytes: Vec<u8>| {
                let patch = MetaPatch {
                    cover: Some(Cover {
                        mime: image_mime(&bytes).into(),
                        data: bytes,
                    }),
                    ..Default::default()
                };
                match tags::apply(&path_main, &patch) {
                    Ok(report) => {
                        if let Some(idx) = a2.current_index() {
                            a2.refresh_track(idx);
                            if let Some(t) = a2.tracks.borrow().get(idx).cloned() {
                                a2.reload_art_pub(&t);
                            }
                        }
                        status2.set_text(&report.summary());
                    }
                    Err(e) => status2.set_text(&format!("写入失败：{e}")),
                }
            };
            if let Some(b) = cached {
                finish(b);
            } else if let Some(url) = c.cover_url {
                status.set_text("正在下载封面…");
                let a3 = a.clone();
                let status3 = status.clone();
                let path2 = path.clone();
                spawn_task(
                    move || online::fetch(&url),
                    move |res| match res {
                        Ok(bytes) => {
                            let patch = MetaPatch {
                                cover: Some(Cover {
                                    mime: image_mime(&bytes).into(),
                                    data: bytes,
                                }),
                                ..Default::default()
                            };
                            match tags::apply(&path2, &patch) {
                                Ok(report) => {
                                    if let Some(idx) = a3.current_index() {
                                        a3.refresh_track(idx);
                                        if let Some(t) = a3.tracks.borrow().get(idx).cloned() {
                                            a3.reload_art_pub(&t);
                                        }
                                    }
                                    status3.set_text(&report.summary());
                                }
                                Err(e) => status3.set_text(&format!("写入失败：{e}")),
                            }
                        }
                        Err(e) => status3.set_text(&format!("封面下载失败：{e}")),
                    },
                );
            } else {
                status.set_text("这条结果没有封面");
            }
        });
    }

    // 选中一条结果 → 带着网络数据进「编辑标签」页，改完在那边保存
    let open_editor: Rc<dyn Fn()> = {
        let a = app.clone();
        let cands = cands.clone();
        let chosen = chosen.clone();
        let cover_bytes = cover_bytes.clone();
        let status = status.clone();
        let path = track.path.clone();
        let w = win.clone();
        Rc::new(move || {
            let Some(i) = *chosen.borrow() else {
                status.set_text("先选一条结果");
                return;
            };
            let cand = cands.borrow().get(i).cloned();
            let Some(c) = cand else { return };

            // 这里只负责把「网络数据 + 封面」递过去；字段校验、封面三态、
            // 写回文件全部复用 open_edit_tags_with，和手动编辑是同一条路径
            let open = {
                let a = a.clone();
                let path = path.clone();
                let c = c.clone();
                Rc::new(move |cover: Option<Vec<u8>>| {
                    let draft = TagDraft {
                        source: c.source.clone(),
                        title: c.title.clone(),
                        artist: c.artist.clone(),
                        album: c.album.clone(),
                        album_artist: c.album_artist.clone(),
                        composer: String::new(),
                        genre: c.genre.clone().unwrap_or_default(),
                        year: c.year,
                        track: c.track,
                        disc: c.disc,
                        cover,
                        cover_url: c.cover_url.clone(),
                    };
                    let idx = a
                        .tracks
                        .borrow()
                        .iter()
                        .position(|t| t.path == path)
                        .or_else(|| a.current_index());
                    open_edit_tags_with(a.clone(), idx, Some(draft));
                })
            };

            if let Some(b) = cover_bytes.borrow().clone() {
                w.close();
                open(Some(b));
            } else if let Some(url) = c.cover_url.clone() {
                status.set_text("正在下载封面…");
                let w2 = w.clone();
                let status2 = status.clone();
                spawn_task(
                    move || online::fetch(&url),
                    move |res| {
                        if res.is_err() {
                            status2.set_text("封面下载失败，先不带封面");
                        }
                        w2.close();
                        open(res.ok());
                    },
                );
            } else {
                w.close();
                open(None);
            }
        })
    };
    {
        let oe = open_editor.clone();
        btn_apply.connect_clicked(move |_| oe());
    }
    // 双击结果行也能直接进编辑页
    {
        let oe = open_editor.clone();
        list.connect_row_activated(move |_, _| oe());
    }
}

/// 供设置面板使用的辅助（未使用但保留以统一入口）
#[allow(dead_code)]
pub fn track_summary(t: &crate::library::Track) -> String {
    format!(
        "{} · {} · {}",
        t.title,
        t.display_artist(),
        library::fmt_time(t.dur_ms)
    )
}

#[allow(dead_code)]
fn unused(_s: LyricStyle) {}

// ================= 手动编辑标签 =================
/// 在线匹配拿到的一条数据，用来预填「编辑标签」页。
///
/// 关键点是：页面、校验、保存**全部复用同一套代码**，唯一不同的只是初值来源。
/// 手动编辑的初值来自源文件，在线匹配的初值来自网络（空字段仍回落到源文件的值，
/// 免得网络没给作曲就把原来文件里的作曲抹掉）。
#[derive(Clone, Default)]
pub struct TagDraft {
    /// 数据来源，显示在页面顶部（iTunes / MusicBrainz …）
    pub source: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_artist: String,
    pub composer: String,
    pub genre: String,
    pub year: Option<u32>,
    pub track: Option<u32>,
    pub disc: Option<u32>,
    /// 已经下载好的封面字节
    pub cover: Option<Vec<u8>>,
    /// 还没下载的封面地址
    pub cover_url: Option<String>,
}

pub fn open_edit_tags(app: Rc<App>, target: Option<usize>) {
    open_edit_tags_with(app, target, None)
}

/// 打开标签编辑页。`draft` 为 `Some` 时表示数据来自网络匹配，
/// 字段用网络值预填，但保存仍走 `tags::apply` 这一条唯一写入路径。
pub fn open_edit_tags_with(app: Rc<App>, target: Option<usize>, draft: Option<TagDraft>) {
    let idx = match target.or_else(|| app.current_index()) {
        Some(i) => i,
        None => {
            app.set_status("先选一首歌再编辑标签");
            return;
        }
    };
    let Some(track) = app.tracks.borrow().get(idx).cloned() else {
        return;
    };
    let from_net = draft.is_some();
    let d = draft.clone().unwrap_or_default();

    let win = gtk::Window::builder()
        .title(if from_net {
            "按在线数据编辑标签"
        } else {
            "编辑标签"
        })
        .transient_for(&app.window)
        .modal(true)
        .default_width(700)
        .default_height(700)
        .build();
    win.add_css_class("sheet");

    let root = gtk::Box::new(gtk::Orientation::Vertical, 14);
    root.set_margin_top(18);
    root.set_margin_bottom(18);
    root.set_margin_start(18);
    root.set_margin_end(18);

    let file_line = gtk::Label::new(Some(&track.path.to_string_lossy()));
    file_line.set_xalign(0.0);
    file_line.set_ellipsize(pango::EllipsizeMode::Middle);
    file_line.add_css_class("tiny");
    root.append(&file_line);

    // 顶部来源说明：明确告诉用户「这是网络来的数据，保存会写进这个文件」
    if from_net {
        let banner = gtk::Box::new(gtk::Orientation::Vertical, 3);
        banner.add_css_class("net-banner");
        let b1 = gtk::Label::new(Some(&format!(
            "数据来源：{}（在线）",
            if d.source.trim().is_empty() {
                "网络"
            } else {
                &d.source
            }
        )));
        b1.set_xalign(0.0);
        b1.add_css_class("net-banner-title");
        let b2 = gtk::Label::new(Some(
            "下面是拉取到的标签，可以直接修改；描边的字段是网络值（和文件里不同），其余是文件原值。保存后写入这个音频文件。",
        ));
        b2.set_xalign(0.0);
        b2.set_wrap(true);
        b2.add_css_class("hint");
        banner.append(&b1);
        banner.append(&b2);
        root.append(&banner);
    }

    let info = section("基本信息");
    let mut entries: Vec<(gtk::Entry, &'static str)> = Vec::new();
    // 网络值优先，空则回落到文件里的原值
    let pick = |net: String, file: String| prefer_net(&net, &file);
    let fields: [(&str, String, &str); 9] = [
        ("曲名", pick(d.title.clone(), track.title.clone()), "歌曲名称"),
        (
            "艺术家",
            pick(d.artist.clone(), track.artist.clone()),
            "歌手 / 社团",
        ),
        ("专辑", pick(d.album.clone(), track.album.clone()), "专辑名"),
        (
            "专辑艺术家",
            pick(d.album_artist.clone(), track.album_artist.clone()),
            "整张专辑的艺术家，可留空",
        ),
        (
            "作曲",
            pick(d.composer.clone(), track.composer.clone()),
            "作曲 / 编曲，可留空",
        ),
        (
            "流派",
            pick(d.genre.clone(), track.genre.clone().unwrap_or_default()),
            "Genre",
        ),
        (
            "年份",
            d.year
                .map(|y| y.to_string())
                .unwrap_or_else(|| track.year.map(|y| y.to_string()).unwrap_or_default()),
            "如 2024",
        ),
        (
            "音轨",
            d.track
                .map(|n| n.to_string())
                .unwrap_or_else(|| track.track_no.map(|n| n.to_string()).unwrap_or_default()),
            "第几首，可留空",
        ),
        (
            "碟号",
            d.disc
                .map(|n| n.to_string())
                .unwrap_or_else(|| track.disc_no.map(|n| n.to_string()).unwrap_or_default()),
            "第几张碟，可留空",
        ),
    ];
    for (label, value, ph) in fields {
        let (b, tail) = row(label);
        let e = gtk::Entry::new();
        e.set_text(&value);
        e.set_placeholder_text(Some(ph));
        e.set_width_chars(26);
        // 来自网络的、和文件里不一样的字段描一下边，一眼看出哪些是新数据
        if from_net {
            let changed = match label {
                "曲名" => !d.title.trim().is_empty() && d.title != track.title,
                "艺术家" => !d.artist.trim().is_empty() && d.artist != track.artist,
                "专辑" => !d.album.trim().is_empty() && d.album != track.album,
                "专辑艺术家" => {
                    !d.album_artist.trim().is_empty() && d.album_artist != track.album_artist
                }
                "流派" => {
                    !d.genre.trim().is_empty()
                        && Some(&d.genre) != track.genre.as_ref()
                }
                "年份" => d.year.is_some() && d.year != track.year,
                "音轨" => d.track.is_some() && d.track != track.track_no,
                "碟号" => d.disc.is_some() && d.disc != track.disc_no,
                _ => false,
            };
            if changed {
                e.add_css_class("from-net");
                e.set_tooltip_text(Some("这一项来自网络"));
            }
        }
        tail.append(&e);
        info.append(&b);
        entries.push((e, ""));
    }
    root.append(&info);

    // 封面：文件的旧封面是「三态判定」的基准，网络封面只作为当前值
    let cover_card = section("专辑封面");
    let cover_row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let preview = gtk::Picture::new();
    preview.set_content_fit(gtk::ContentFit::Cover);
    preview.set_size_request(84, 84);
    let frame = gtk::Box::new(gtk::Orientation::Vertical, 0);
    frame.add_css_class("cover-frame");
    frame.set_size_request(84, 84);
    frame.append(&preview);
    let chosen_cover: Rc<RefCell<Option<Vec<u8>>>> = Rc::new(RefCell::new(None));
    let file_cover = tags::read_cover(&track.path);
    if let Some(c) = &file_cover {
        if let Some(w) = art::from_bytes(&c.data, 0.6) {
            let thumb = art::square(&w.cover, 84);
            preview.set_paintable(thumb.as_ref());
        }
        *chosen_cover.borrow_mut() = Some(c.data.clone());
    }
    // 记下原始封面，保存时才能区分「没动」「换了一张」「删掉」
    let orig_cover: Option<Vec<u8>> = file_cover.as_ref().map(|c| c.data.clone());
    if let Some(bytes) = &d.cover {
        if let Some(w) = art::from_bytes(bytes, 0.6) {
            let thumb = art::square(&w.cover, 84);
            preview.set_paintable(thumb.as_ref());
        }
        *chosen_cover.borrow_mut() = Some(bytes.clone());
    } else if let Some(url) = d.cover_url.clone() {
        // 只有在打开页面时还没下载过才补一次
        let chosen = chosen_cover.clone();
        let pv = preview.clone();
        spawn_task(
            move || online::fetch(&url),
            move |res| {
                if let Ok(bytes) = res {
                    if let Some(w) = art::from_bytes(&bytes, 0.6) {
                        let thumb = art::square(&w.cover, 84);
                        pv.set_paintable(thumb.as_ref());
                        *chosen.borrow_mut() = Some(bytes);
                    }
                }
            },
        );
    }
    let cover_btns = gtk::Box::new(gtk::Orientation::Vertical, 6);
    cover_btns.set_valign(gtk::Align::Center);
    let btn_pick = gtk::Button::with_label("选择本地图片…");
    btn_pick.add_css_class("ghost");
    let btn_clear = gtk::Button::with_label("移除封面");
    btn_clear.add_css_class("ghost");
    cover_btns.append(&btn_pick);
    cover_btns.append(&btn_clear);
    cover_row.append(&frame);
    cover_row.append(&cover_btns);
    cover_card.append(&cover_row);
    root.append(&cover_card);

    {
        let w = win.clone();
        let chosen = chosen_cover.clone();
        let pv = preview.clone();
        btn_pick.connect_clicked(move |_| {
            let dlg = gtk::FileDialog::builder()
                .title("选择封面图片")
                .modal(true)
                .build();
            let chosen = chosen.clone();
            let pv = pv.clone();
            let filters = gio::ListStore::new::<gtk::FileFilter>();
            let f = gtk::FileFilter::new();
            f.set_name(Some("图片"));
            f.add_mime_type("image/*");
            filters.append(&f);
            dlg.set_filters(Some(&filters));
            dlg.open(Some(&w), None::<&gio::Cancellable>, move |res| {
                if let Ok(file) = res {
                    if let Some(p) = file.path() {
                        if let Ok(bytes) = std::fs::read(&p) {
                            if let Some(w) = art::from_bytes(&bytes, 0.6) {
                                let thumb = art::square(&w.cover, 84);
                                pv.set_paintable(thumb.as_ref());
                                *chosen.borrow_mut() = Some(bytes);
                            }
                        }
                    }
                }
            });
        });
    }
    {
        let chosen = chosen_cover.clone();
        let pv = preview.clone();
        btn_clear.connect_clicked(move |_| {
            *chosen.borrow_mut() = None;
            pv.set_paintable(None::<&gtk::gdk::Texture>);
        });
    }

    // 歌词
    let lyr_card = section("歌词（LRC）");
    let tv = gtk::TextView::new();
    tv.set_wrap_mode(gtk::WrapMode::WordChar);
    tv.set_monospace(true);
    let buf = tv.buffer();
    let existing = std::fs::read_to_string(track.path.with_extension("lrc"))
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| tags::read_lyrics(&track.path))
        .unwrap_or_default();
    buf.set_text(&existing);
    let lyr_scroll = gtk::ScrolledWindow::new();
    lyr_scroll.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
    lyr_scroll.set_min_content_height(150);
    lyr_scroll.set_child(Some(&tv));
    lyr_scroll.add_css_class("card");
    lyr_card.append(&lyr_scroll);
    let lyr_hint = gtk::Label::new(Some(
        "可以直接粘贴带时间轴的 LRC，例如 [00:12.34]歌词；留空表示不修改。",
    ));
    lyr_hint.set_xalign(0.0);
    lyr_hint.set_wrap(true);
    lyr_hint.add_css_class("hint");
    lyr_card.append(&lyr_hint);
    root.append(&lyr_card);

    // 写入策略
    let options = section("保存方式");
    let (ob, ot) = row("同时写入音频文件");
    let sw_write = gtk::Switch::new();
    // 在线数据默认就写回文件：用户点「应用」的意图就是更新这个文件；
    // 手动编辑仍按老习惯默认不碰磁盘，想写自己打开。
    sw_write.set_active(from_net);
    sw_write.set_valign(gtk::Align::Center);
    ot.append(&sw_write);
    options.append(&ob);
    let opt_hint = gtk::Label::new(Some(if from_net {
        "打开时会把上面的标签、封面、歌词写回源文件（保存时统一按方言层重建，ID3 一律升级到 v2.4）；关掉则只改本次运行里的显示。"
    } else {
        "关闭时改动只在本次运行中生效，不会碰音频文件；打开后会把标签、封面、歌词写回原文件，写入前请备份。"
    }));
    opt_hint.set_xalign(0.0);
    opt_hint.set_wrap(true);
    opt_hint.add_css_class("hint");
    options.append(&opt_hint);
    root.append(&options);

    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    actions.set_halign(gtk::Align::End);
    let btn_cancel = gtk::Button::with_label("取消");
    btn_cancel.add_css_class("ghost");
    let btn_save = gtk::Button::with_label("保存");
    btn_save.add_css_class("accent");
    actions.append(&btn_cancel);
    actions.append(&btn_save);
    // 按钮区固定在窗口底部：字段多，滚到底才看见「保存」太别扭
    actions.set_margin_start(18);
    actions.set_margin_end(18);
    actions.set_margin_bottom(14);

    let scroll = gtk::ScrolledWindow::new();
    scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scroll.set_child(Some(&root));
    scroll.set_vexpand(true);
    let outer = gtk::Box::new(gtk::Orientation::Vertical, 8);
    outer.append(&scroll);
    outer.append(&actions);
    win.set_child(Some(&outer));
    win.present();

    {
        let w = win.clone();
        btn_cancel.connect_clicked(move |_| w.close());
    }

    {
        let a = app.clone();
        let w = win.clone();
        let entries = entries.clone();
        // TextView buffer 不实现 Clone 语义上的移动，直接克隆引用
        let buffer = buf.clone();
        let write = sw_write.clone();
        let cover = chosen_cover.clone();
        let path = track.path.clone();
        // 记下原值：只提交真正改过的字段。
        // 「某个字段该怎么写」由方言层决定，这里不再为年份之类开特例。
        let orig = track.clone();
        let orig_cover = orig_cover.clone();
        btn_save.connect_clicked(move |_| {
            let text_of = |i: usize| {
                entries
                    .get(i)
                    .map(|(e, _)| e.text().to_string())
                    .unwrap_or_default()
            };
            // 输入框顺序：曲名 0 / 艺术家 1 / 专辑 2 / 专辑艺术家 3 / 作曲 4 /
            //              流派 5 / 年份 6 / 音轨 7 / 碟号 8
            let title_text = text_of(0);
            let artist_text = text_of(1);
            let album_text = text_of(2);
            let album_artist_text = text_of(3);
            let composer_text = text_of(4);
            let genre_text = text_of(5);
            let year_text = text_of(6);
            let track_text = text_of(7);
            let disc_text = text_of(8);
            let digits_u32 = |s: &str| -> Option<u32> {
                s.trim()
                    .chars()
                    .filter(|c| c.is_ascii_digit())
                    .collect::<String>()
                    .parse::<u32>()
                    .ok()
            };
            let year = digits_u32(&year_text);
            let track_no = digits_u32(&track_text);
            let disc_no = digits_u32(&disc_text);

            // 封面三态：没动 / 换了一张 / 删掉
            let cur_cover = cover.borrow().clone();
            let (cover_patch, remove_cover) = match (&orig_cover, &cur_cover) {
                (Some(_), None) => (None, true),
                (before, Some(now)) if before.as_ref() != Some(now) => (
                    Some(Cover {
                        data: now.clone(),
                        mime: image_mime(now).into(),
                    }),
                    false,
                ),
                _ => (None, false),
            };

            let orig_genre = orig.genre.clone().unwrap_or_default();
            let mut patch = MetaPatch {
                title: (title_text != orig.title).then_some(title_text),
                artist: (artist_text != orig.artist).then_some(artist_text),
                album: (album_text != orig.album).then_some(album_text),
                album_artist: (album_artist_text != orig.album_artist)
                    .then_some(album_artist_text),
                composer: (composer_text != orig.composer).then_some(composer_text),
                genre: (genre_text != orig_genre).then_some(genre_text),
                year: year.filter(|y| Some(*y) != orig.year),
                track: track_no.filter(|n| Some(*n) != orig.track_no),
                disc: disc_no.filter(|n| Some(*n) != orig.disc_no),
                cover: cover_patch,
                remove_cover,
                ..Default::default()
            };
            let lyrics = buffer
                .text(&buffer.start_iter(), &buffer.end_iter(), false)
                .to_string();
            let lyrics_changed = !lyrics.trim().is_empty() && lyrics != existing;
            if lyrics_changed {
                patch.lyrics = Some(lyrics.clone());
            }

            if patch.is_empty() {
                a.set_status("没有改动");
                w.close();
                return;
            }

            if write.is_active() {
                match tags::apply(&path, &patch) {
                    Ok(report) => {
                        if lyrics_changed {
                            // 侧车 LRC 方便用户直接编辑；标签内的歌词已在 patch 里
                            let _ = std::fs::write(path.with_extension("lrc"), &lyrics);
                        }
                        a.refresh_track(idx);
                        if let Some(t) = a.tracks.borrow().get(idx).cloned() {
                            a.reload_art_pub(&t);
                        }
                        a.set_status(&report.summary());
                    }
                    Err(e) => {
                        a.set_status(&format!("写入失败：{e}（文件可能是只读的）"));
                        return;
                    }
                }
            } else {
                {
                    let mut tracks = a.tracks.borrow_mut();
                    if let Some(t) = tracks.get_mut(idx) {
                        if let Some(v) = &patch.title {
                            t.title = v.clone();
                        }
                        if let Some(v) = &patch.artist {
                            t.artist = v.clone();
                        }
                        if let Some(v) = &patch.album {
                            t.album = v.clone();
                        }
                        if let Some(v) = &patch.album_artist {
                            t.album_artist = v.clone();
                        }
                        if let Some(v) = &patch.composer {
                            t.composer = v.clone();
                        }
                        if let Some(v) = &patch.genre {
                            t.genre = Some(v.clone());
                        }
                        if let Some(v) = patch.year {
                            t.year = Some(v);
                        }
                        if let Some(v) = patch.track {
                            t.track_no = Some(v);
                        }
                        if let Some(v) = patch.disc {
                            t.disc_no = Some(v);
                        }
                        if patch.remove_cover {
                            t.has_cover = false;
                        } else if patch.cover.is_some() {
                            t.has_cover = true;
                        }
                    }
                }
                if lyrics_changed {
                    a.set_lyrics(crate::lrc::parse(&lyrics));
                }
                // 内存模式也要让底栏/列表跟着变，否则改了曲名却还显示旧的
                if a.current.get() == Some(idx) {
                    if let Some(t) = a.tracks.borrow().get(idx).cloned() {
                        a.update_now_playing_pub(&t);
                        a.reload_art_pub(&t);
                    }
                }
                let q = a.search.text().to_string();
                a.apply_filter(&q);
                a.set_status("已应用（仅本次运行，未改动文件）");
            }
            w.close();
        });
    }
}

/// 预填规则：网络给了值就用网络值，没给（空串）就保留文件里的原值。
/// 这条规则很重要——不能因为 iTunes 没有「作曲」，就把文件里原来的作曲清空。
fn prefer_net(net: &str, file: &str) -> String {
    if net.trim().is_empty() {
        file.to_string()
    } else {
        net.to_string()
    }
}

/// 封面 MIME：按文件头判断，别一律当 jpeg 写进 APIC 帧
fn image_mime(bytes: &[u8]) -> &'static str {    if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        "image/png"
    } else if bytes.len() > 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        "image/webp"
    } else if bytes.starts_with(b"GIF8") {
        "image/gif"
    } else {
        "image/jpeg"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 网络没给的字段必须回落到文件原值，不能把原有标签抹成空
    #[test]
    fn net_value_wins_only_when_present() {
        assert_eq!(prefer_net("YOASOBI", "未知艺人"), "YOASOBI");
        assert_eq!(prefer_net("", "泽野弘之"), "泽野弘之");
        assert_eq!(prefer_net("   ", "泽野弘之"), "泽野弘之");
        // 全是空白也算没给
        assert_eq!(prefer_net("\t\n", "原值"), "原值");
    }

    /// 封面 MIME 按文件头判断，写进 APIC 帧的 mime 才不会错
    #[test]
    fn cover_mime_sniffs_magic_bytes() {
        assert_eq!(image_mime(&[0xFF, 0xD8, 0xFF, 0xE0]), "image/jpeg");
        assert_eq!(image_mime(&[0x89, b'P', b'N', b'G', 0x0D]), "image/png");
        assert_eq!(
            image_mime(b"RIFF\x24\x00\x00\x00WEBPVP8 "),
            "image/webp"
        );
        assert_eq!(image_mime(b"GIF89a"), "image/gif");
    }
}
