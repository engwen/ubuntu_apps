//! 对话框：设置、歌词样式、在线歌词、在线元数据匹配。

use super::{art, spawn_task, App};
use crate::config::LyricStyle;
use crate::library;
use crate::lrc;
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
        .default_width(560)
        .default_height(620)
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
                if let Some(t) = a.current_track() {
                    if let Some(c) = tags::read_cover(&t.path) {
                        if let Some(w) = art::from_bytes(&c.data, a.cfg.borrow().blur) {
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
            sf.set_active(true);
            if let Some(idx) = a.current_index() {
                let path = a.tracks.borrow()[idx].path.clone();
                if let Some(c) = tags::read_cover(&path) {
                    if let Some(w) = art::from_bytes(&c.data, a.cfg.borrow().blur) {
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

    let (rb5, tb5) = row("没有歌词时自动在线匹配");
    let sw_auto = gtk::Switch::new();
    sw_auto.set_active(app.cfg.borrow().auto_lyric);
    sw_auto.set_valign(gtk::Align::Center);
    tb5.append(&sw_auto);
    behavior.append(&rb5);
    {
        let a = app.clone();
        sw_auto.connect_active_notify(move |s| {
            let mut c = a.cfg.borrow_mut();
            c.auto_lyric = s.is_active();
            c.save();
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

pub fn fetch_online_lyric(app: Rc<App>) {
    let Some(t) = app.current_track() else {
        app.set_status("先选一首歌再搜索歌词");
        return;
    };
    app.set_status("正在 LRCLIB 搜索歌词…");
    let artist = t.artist.clone();
    let title = t.title.clone();
    let album = t.album.clone();
    let dur = t.dur_ms;
    let path = t.path.clone();
    let a = app.clone();
    spawn_task(
        move || online::fetch_lyrics(&artist, &title, &album, dur),
        move |res| match res {
            Ok(hit) => {
                let lyr = lrc::parse(&hit.lrc);
                let n = lyr.lines.len();
                let synced = lyr.synced;
                a.set_lyrics(lyr);
                // 顺手存成同名 .lrc，方便其它播放器共用
                let side = path.with_extension("lrc");
                let _ = std::fs::write(&side, &hit.lrc);
                a.set_status(&format!(
                    "已获取{}歌词（{} 行，来源 {}），可在歌词栏把歌词写入标签",
                    if synced { "时间轴" } else { "纯文本" },
                    n,
                    hit.source
                ));
            }
            Err(e) => a.set_status(&format!("歌词获取失败：{e}")),
        },
    );
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

    let status = gtk::Label::new(Some("输入关键词后点搜索，选一条结果再应用"));
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
    let btn_apply = gtk::Button::with_label("应用并写入标签");
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
                        data: bytes,
                        mime: "image/jpeg".into(),
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
                                    data: bytes,
                                    mime: "image/jpeg".into(),
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

    // 应用并写入
    {
        let a = app.clone();
        let cands = cands.clone();
        let chosen = chosen.clone();
        let cover_bytes = cover_bytes.clone();
        let status = status.clone();
        let path = track.path.clone();
        btn_apply.connect_clicked(move |_| {
            let Some(i) = *chosen.borrow() else {
                status.set_text("先选一条结果");
                return;
            };
            let cand = cands.borrow().get(i).cloned();
            let Some(c) = cand else { return };
            let mut patch = MetaPatch {
                title: Some(c.title.clone()).filter(|s| !s.trim().is_empty()),
                artist: Some(c.artist.clone()).filter(|s| !s.trim().is_empty()),
                album: Some(c.album.clone()).filter(|s| !s.trim().is_empty()),
                album_artist: Some(c.album_artist.clone()).filter(|s| !s.trim().is_empty()),
                year: c.year,
                track: c.track,
                disc: c.disc,
                genre: c.genre.clone().filter(|s| !s.trim().is_empty()),
                cover: cover_bytes.borrow().clone().map(|b| Cover {
                    data: b,
                    mime: "image/jpeg".into(),
                }),
                ..Default::default()
            };
            // 没有现成封面就先下载
            if patch.cover.is_none() {
                if let Some(url) = c.cover_url.clone() {
                    status.set_text("正在下载封面…");
                    let a2 = a.clone();
                    let status2 = status.clone();
                    let path2 = path.clone();
                    spawn_task(
                        move || online::fetch(&url),
                        move |res| {
                            if let Ok(bytes) = res {
                                patch.cover = Some(Cover {
                                    data: bytes,
                                    mime: "image/jpeg".into(),
                                });
                            }
                            match tags::apply(&path2, &patch) {
                                Ok(report) => {
                                    if let Some(idx) = a2.current_index() {
                                        a2.refresh_track(idx);
                                        if let Some(t) = a2.tracks.borrow().get(idx).cloned() {
                                            a2.update_now_playing_pub(&t);
                                            a2.reload_art_pub(&t);
                                        }
                                    }
                                    status2.set_text(&report.summary());
                                }
                                Err(e) => status2.set_text(&format!("写入失败：{e}")),
                            }
                        },
                    );
                    return;
                }
            }
            match tags::apply(&path, &patch) {
                Ok(report) => {
                    if let Some(idx) = a.current_index() {
                        a.refresh_track(idx);
                        if let Some(t) = a.tracks.borrow().get(idx).cloned() {
                            a.update_now_playing_pub(&t);
                            a.reload_art_pub(&t);
                        }
                    }
                    status.set_text(&report.summary());
                }
                Err(e) => status.set_text(&format!("写入失败：{e}")),
            }
        });
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
pub fn open_edit_tags(app: Rc<App>, target: Option<usize>) {
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

    let win = gtk::Window::builder()
        .title("编辑标签")
        .transient_for(&app.window)
        .modal(true)
        .default_width(560)
        .default_height(660)
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

    let info = section("基本信息");
    let mut entries: Vec<(gtk::Entry, &'static str)> = Vec::new();
    let fields: [(&str, String, &str); 6] = [
        ("曲名", track.title.clone(), "歌曲名称"),
        ("艺术家", track.artist.clone(), "歌手 / 社团"),
        ("专辑", track.album.clone(), "专辑名"),
        ("作曲", track.composer.clone(), "作曲 / 编曲，可留空"),
        ("流派", track.genre.clone().unwrap_or_default(), "Genre"),
        (
            "年份",
            track.year.map(|y| y.to_string()).unwrap_or_default(),
            "如 2024",
        ),
    ];
    for (label, value, ph) in fields {
        let (b, tail) = row(label);
        let e = gtk::Entry::new();
        e.set_text(&value);
        e.set_placeholder_text(Some(ph));
        e.set_width_chars(26);
        tail.append(&e);
        info.append(&b);
        entries.push((e, ""));
    }
    root.append(&info);

    // 封面
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
    if let Some(c) = tags::read_cover(&track.path) {
        if let Some(w) = art::from_bytes(&c.data, 0.6) {
            let thumb = art::square(&w.cover, 84);
            preview.set_paintable(thumb.as_ref());
        }
        *chosen_cover.borrow_mut() = Some(c.data);
    }
    // 记下原始封面，保存时才能区分「没动」「换了一张」「删掉」
    let orig_cover: Option<Vec<u8>> = chosen_cover.borrow().clone();
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
    sw_write.set_active(false);
    sw_write.set_valign(gtk::Align::Center);
    ot.append(&sw_write);
    options.append(&ob);
    let opt_hint = gtk::Label::new(Some(
        "关闭时改动只在本次运行中生效，不会碰音频文件；打开后会把标签、封面、歌词写回原文件，写入前请备份。",
    ));
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
    root.append(&actions);

    let scroll = gtk::ScrolledWindow::new();
    scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scroll.set_child(Some(&root));
    win.set_child(Some(&scroll));
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
            // 输入框顺序：曲名 0 / 艺术家 1 / 专辑 2 / 作曲 3 / 流派 4 / 年份 5
            let title_text = text_of(0);
            let artist_text = text_of(1);
            let album_text = text_of(2);
            let composer_text = text_of(3);
            let genre_text = text_of(4);
            let year_text = text_of(5);
            let year = year_text
                .trim()
                .chars()
                .filter(|c| c.is_ascii_digit())
                .collect::<String>()
                .parse::<u32>()
                .ok();

            // 封面三态：没动 / 换了一张 / 删掉
            let cur_cover = cover.borrow().clone();
            let (cover_patch, remove_cover) = match (&orig_cover, &cur_cover) {
                (Some(_), None) => (None, true),
                (before, Some(now)) if before.as_ref() != Some(now) => (
                    Some(Cover {
                        data: now.clone(),
                        mime: "image/jpeg".into(),
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
                composer: (composer_text != orig.composer).then_some(composer_text),
                genre: (genre_text != orig_genre).then_some(genre_text),
                year: year.filter(|y| Some(*y) != orig.year),
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
                        if let Some(v) = &patch.composer {
                            t.composer = v.clone();
                        }
                        if let Some(v) = &patch.genre {
                            t.genre = Some(v.clone());
                        }
                        if let Some(v) = patch.year {
                            t.year = Some(v);
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
                let q = a.search.text().to_string();
                a.apply_filter(&q);
                a.set_status("已应用（仅本次运行，未改动文件）");
            }
            w.close();
        });
    }
}
