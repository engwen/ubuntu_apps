//! 统一主题：整窗一套配色。
//!
//! 早期版本工具栏、HeaderBar、状态栏跟随系统主题，而编辑区由 GSK 用
//! 硬编码的深色画出来，于是浅色系统下出现「工具栏白、内容黑」的割裂。
//! 这里改为：用一个应用级 CssProvider 接管窗口内所有控件的配色，
//! 编辑区自绘用的 RGBA 与 CSS 中的 `@define-color` 逐项取自同一份 Theme，
//! 无论系统主题明暗，窗口内部始终一致。

use gtk::gdk;

fn rgba(r: f32, g: f32, b: f32, a: f32) -> gdk::RGBA {
    gdk::RGBA::new(r, g, b, a)
}

fn hex(c: &gdk::RGBA) -> String {
    let f = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02x}{:02x}{:02x}", f(c.red()), f(c.green()), f(c.blue()))
}

#[derive(Clone)]
pub struct Theme {
    pub name: &'static str,
    /// 编辑区正文背景（GSK 自绘）
    pub bg: gdk::RGBA,
    pub gutter_bg: gdk::RGBA,
    pub gutter_fg: gdk::RGBA,
    pub fg: gdk::RGBA,
    pub current_line: gdk::RGBA,
    pub selection: gdk::RGBA,
    pub cursor: gdk::RGBA,
    /// 预编辑串的**下划线**颜色（低对比度灰）。文字本身直接用 `fg`，
    /// 不再给预编辑串单独配色——之前用浅蓝 `#8cd9ff`，用户明确要求
    /// 「显示白色的那种就行了」，蓝色字 + 蓝色下划线反而像另起一套主题。
    pub preedit_underline: gdk::RGBA,
    pub match_hl: gdk::RGBA,
    /// 界面控件配色（CSS）
    pub chrome: gdk::RGBA,
    pub chrome_fg: gdk::RGBA,
    pub button: gdk::RGBA,
    pub button_hover: gdk::RGBA,
    pub field: gdk::RGBA,
    pub border: gdk::RGBA,
    pub scroll: gdk::RGBA,
    pub scroll_hover: gdk::RGBA,
    pub accent: gdk::RGBA,
    pub dim: gdk::RGBA,
}

impl Theme {
    pub fn dark() -> Self {
        Self {
            name: "dark",
            bg: rgba(0.129, 0.141, 0.176, 1.0),
            gutter_bg: rgba(0.153, 0.165, 0.204, 1.0),
            gutter_fg: rgba(0.48, 0.51, 0.59, 1.0),
            fg: rgba(0.88, 0.90, 0.94, 1.0),
            current_line: rgba(0.180, 0.196, 0.243, 1.0),
            selection: rgba(0.24, 0.44, 0.76, 0.55),
            cursor: rgba(0.98, 0.79, 0.36, 1.0),
            preedit_underline: rgba(0.60, 0.63, 0.70, 0.65),
            match_hl: rgba(0.95, 0.72, 0.25, 0.38),
            chrome: rgba(0.153, 0.165, 0.204, 1.0),
            chrome_fg: rgba(0.88, 0.90, 0.94, 1.0),
            button: rgba(0.212, 0.227, 0.278, 1.0),
            button_hover: rgba(0.267, 0.286, 0.345, 1.0),
            field: rgba(0.106, 0.118, 0.153, 1.0),
            border: rgba(0.267, 0.286, 0.345, 1.0),
            scroll: rgba(0.349, 0.373, 0.439, 1.0),
            scroll_hover: rgba(0.451, 0.478, 0.549, 1.0),
            accent: rgba(0.24, 0.48, 0.82, 1.0),
            dim: rgba(0.60, 0.63, 0.70, 1.0),
        }
    }

    pub fn light() -> Self {
        Self {
            name: "light",
            bg: rgba(1.0, 1.0, 1.0, 1.0),
            gutter_bg: rgba(0.965, 0.969, 0.976, 1.0),
            gutter_fg: rgba(0.49, 0.51, 0.55, 1.0),
            fg: rgba(0.13, 0.15, 0.18, 1.0),
            current_line: rgba(0.949, 0.961, 0.980, 1.0),
            selection: rgba(0.42, 0.65, 0.96, 0.45),
            cursor: rgba(0.10, 0.35, 0.85, 1.0),
            preedit_underline: rgba(0.42, 0.45, 0.50, 0.65),
            match_hl: rgba(1.0, 0.82, 0.30, 0.55),
            chrome: rgba(0.965, 0.969, 0.976, 1.0),
            chrome_fg: rgba(0.13, 0.15, 0.18, 1.0),
            button: rgba(1.0, 1.0, 1.0, 1.0),
            button_hover: rgba(0.910, 0.929, 0.953, 1.0),
            field: rgba(1.0, 1.0, 1.0, 1.0),
            border: rgba(0.855, 0.871, 0.898, 1.0),
            scroll: rgba(0.741, 0.765, 0.800, 1.0),
            scroll_hover: rgba(0.600, 0.631, 0.678, 1.0),
            accent: rgba(0.16, 0.42, 0.82, 1.0),
            dim: rgba(0.42, 0.45, 0.50, 1.0),
        }
    }

    pub fn by_name(name: &str) -> Self {
        if name == "light" { Self::light() } else { Self::dark() }
    }

    /// 生成整窗 CSS。`@define-color` 的值与上面的 RGBA 字段同源。
    pub fn css(&self) -> String {
        format!(
            r#"
@define-color owl_bg {bg};
@define-color owl_chrome {chrome};
@define-color owl_fg {fg};
@define-color owl_button {button};
@define-color owl_button_hover {button_hover};
@define-color owl_field {field};
@define-color owl_border {border};
@define-color owl_scroll {scroll};
@define-color owl_scroll_hover {scroll_hover};
@define-color owl_accent {accent};
@define-color owl_dim {dim};

window.owleditor {{
  background-color: @owl_bg;
  color: @owl_fg;
  background-image: none;
}}
window.owleditor headerbar,
window.owleditor .owl-chrome {{
  background-color: @owl_chrome;
  background-image: none;
  color: @owl_fg;
  border-color: @owl_border;
}}
/* 顶栏高度：GTK4 默认 HeaderBar 约 46px（6px 内边距 + 34px 按钮）。
   菜单栏、工具按钮、窗口控制按钮三者叠加后显得过高；这里把整条压到
   约 32px，只收内边距与最小高度，不动字号，图标和文字仍可点。 */
window.owleditor headerbar {{
  border-bottom: 1px solid @owl_border;
  min-height: 30px;
  padding-top: 0;
  padding-bottom: 0;
  padding-left: 4px;
  padding-right: 4px;
}}
window.owleditor headerbar button {{
  min-height: 22px;
  padding: 1px 6px;
}}
window.owleditor menubar > item {{
  min-height: 22px;
  padding: 1px 6px;
}}
/* 菜单栏与工具按钮同处一条 HeaderBar：工具条本身不再画第二条带边框的横条 */
window.owleditor .owl-toolbar {{
  background-color: transparent;
  background-image: none;
  border: none;
  padding: 0 2px;
}}
window.owleditor .owl-toolbar separator {{ margin: 2px 3px; min-width: 1px; }}
window.owleditor .owl-status {{
  background-color: @owl_chrome;
  border-top: 1px solid @owl_border;
  color: @owl_dim;
  padding: 3px 10px;
}}
/* 查找/替换浮窗：叠在编辑区右上角的小卡片，不是压在正文上方的横栏。
   需要圆角 + 描边 + 阴影，让它和底下的正文明确分层。 */
window.owleditor .owl-findbar {{
  background-color: @owl_chrome;
  background-image: none;
  border: 1px solid @owl_border;
  border-radius: 8px;
  padding: 8px 10px;
  box-shadow: 0 6px 16px rgba(0, 0, 0, 0.25);
}}
window.owleditor .owl-findbar entry {{ min-height: 26px; }}
window.owleditor .owl-findbar searchentry {{ min-height: 26px; }}
window.owleditor .owl-find-nav {{
  min-width: 24px;
  min-height: 24px;
  padding: 1px;
}}
window.owleditor .owl-find-toggle {{
  min-width: 26px;
  min-height: 24px;
  padding: 1px 6px;
  font-size: 12px;
}}
window.owleditor .owl-find-toggle:checked {{
  background-color: @owl_accent;
  border-color: @owl_accent;
  color: white;
}}
window.owleditor .owl-find-btn {{
  min-height: 26px;
  padding: 2px 10px;
}}
/* 标签栏：与顶栏同一条色带，选中页用正文底色，未选中页压暗。
   不显式接管的话，Adwaita 的浅色标签会在深色窗口里露出一块白。 */
window.owleditor notebook {{ background-color: @owl_bg; }}
window.owleditor notebook > header {{
  background-color: @owl_chrome;
  background-image: none;
  border-bottom: 1px solid @owl_border;
  padding: 2px 4px 0 4px;
}}
window.owleditor notebook tab {{
  background-color: transparent;
  background-image: none;
  border: 1px solid transparent;
  border-bottom: none;
  border-radius: 6px 6px 0 0;
  padding: 3px 8px;
  margin: 0 2px;
  min-height: 20px;
  color: @owl_dim;
}}
window.owleditor notebook tab:hover {{ background-color: @owl_button; }}
window.owleditor notebook tab:checked,
window.owleditor notebook tab:active {{
  background-color: @owl_bg;
  border-color: @owl_border;
}}
window.owleditor notebook tab label {{ color: @owl_dim; padding: 0; }}
window.owleditor notebook tab:hover label {{ color: @owl_fg; }}
window.owleditor notebook tab:checked label {{ color: @owl_fg; }}
window.owleditor .owl-tab-close {{
  min-width: 16px;
  min-height: 16px;
  padding: 0;
  margin: 0;
  border: none;
  border-radius: 4px;
}}
window.owleditor label {{ color: @owl_fg; }}
window.owleditor .owl-status label {{ color: @owl_dim; }}
window.owleditor .owl-dim {{ color: @owl_dim; }}
window.owleditor button {{
  background-color: @owl_button;
  background-image: none;
  color: @owl_fg;
  border: 1px solid @owl_border;
  border-radius: 6px;
  padding: 4px 10px;
  box-shadow: none;
  text-shadow: none;
}}
window.owleditor button:hover {{ background-color: @owl_button_hover; }}
window.owleditor button:checked {{ background-color: @owl_accent; color: white; }}
window.owleditor button:disabled {{ opacity: 0.45; }}
window.owleditor button.flat {{
  background-color: transparent;
  border-color: transparent;
  box-shadow: none;
}}
window.owleditor button.flat:hover {{ background-color: @owl_button_hover; }}
window.owleditor entry,
window.owleditor searchentry {{
  background-color: @owl_field;
  background-image: none;
  color: @owl_fg;
  border: 1px solid @owl_border;
  border-radius: 6px;
}}
window.owleditor entry:focus-within {{ border-color: @owl_accent; }}
window.owleditor checkbutton {{ color: @owl_fg; }}
window.owleditor separator {{ background-color: @owl_border; }}
/* 独立 GtkScrollbar 必须给足尺寸，否则滑块可能只有 0 宽/0 高，看得见却拖不动 */
window.owleditor scrollbar {{
  background-color: @owl_chrome;
  min-width: 14px;
  min-height: 14px;
}}
window.owleditor scrollbar trough {{
  background-color: @owl_field;
  border-radius: 8px;
}}
window.owleditor scrollbar slider {{
  background-color: @owl_scroll;
  border-radius: 8px;
  min-width: 12px;
  min-height: 12px;
}}
window.owleditor scrollbar slider:hover {{ background-color: @owl_scroll_hover; }}
window.owleditor .owl-tool {{
  min-width: 22px;
  min-height: 22px;
  padding: 2px;
}}
window.owleditor .owl-editor {{ background-color: @owl_bg; }}
/* 菜单弹层：hover / focus / selected / active / disabled 全部显式接管。
   只写 :hover 是不够的 —— 键盘打开菜单时首项处于 :focus 状态，
   GTK 默认主题（Adwaita）会给它浅色底 + 深色字；本项目又把菜单文字
   统一改成浅色，两者叠加就变成「白底浅字」看不见。
   所以底色与字色必须成对声明，且要把内部 label / accelerator 一起覆盖。 */
window.owleditor popover,
window.owleditor popover contents,
window.owleditor popover.menu,
window.owleditor popover.menu > contents {{
  background-color: @owl_chrome;
  background-image: none;
  color: @owl_fg;
}}
window.owleditor popover.menu > contents {{
  border: 1px solid @owl_border;
  border-radius: 8px;
}}
window.owleditor popover menuitem,
window.owleditor popover modelbutton {{
  background-color: transparent;
  background-image: none;
  color: @owl_fg;
  padding: 3px 10px;
  border-radius: 5px;
}}
window.owleditor popover menuitem:hover,
window.owleditor popover menuitem:focus,
window.owleditor popover menuitem:selected,
window.owleditor popover menuitem:active,
window.owleditor popover modelbutton:hover,
window.owleditor popover modelbutton:focus,
window.owleditor popover modelbutton:selected,
window.owleditor popover modelbutton:active,
window.owleditor popover modelbutton:focus-within {{
  background-color: @owl_button_hover;
  background-image: none;
  color: @owl_fg;
  box-shadow: none;
}}
window.owleditor popover menuitem label,
window.owleditor popover modelbutton label,
window.owleditor popover menuitem:hover label,
window.owleditor popover menuitem:focus label,
window.owleditor popover modelbutton:hover label,
window.owleditor popover modelbutton:focus label,
window.owleditor popover modelbutton:selected label {{
  color: @owl_fg;
}}
window.owleditor popover modelbutton accelerator,
window.owleditor popover menuitem accelerator {{
  color: @owl_dim;
}}
window.owleditor popover menuitem:disabled,
window.owleditor popover modelbutton:disabled {{
  background-color: transparent;
  color: @owl_dim;
  opacity: 0.55;
}}
window.owleditor popover separator {{
  background-color: @owl_border;
  min-height: 1px;
}}
window.owleditor menubar {{ background-color: @owl_chrome; color: @owl_fg; }}
window.owleditor menubar > item {{ color: @owl_fg; }}
"#,
            bg = hex(&self.bg),
            chrome = hex(&self.chrome),
            fg = hex(&self.fg),
            button = hex(&self.button),
            button_hover = hex(&self.button_hover),
            field = hex(&self.field),
            border = hex(&self.border),
            scroll = hex(&self.scroll),
            scroll_hover = hex(&self.scroll_hover),
            accent = hex(&self.accent),
            dim = hex(&self.dim),
        )
    }
}

/// 安装/替换应用级 CSS provider。切换主题时用同一个 provider 重新 load，
/// 避免重复叠加导致优先级混乱。
pub fn install(display: &gdk::Display, provider: &gtk::CssProvider, theme: &Theme) {
    // 让 GTK 的默认主题与应用配色一致：深色时优先深色。
    // 否则菜单、对话框里未被上面 CSS 覆盖到的默认状态会按浅色主题取值
    // （浅底 + 深字），与我们的浅色文字叠加成不可读的白底浅字。
    if let Some(settings) = gtk::Settings::default() {
        settings.set_gtk_application_prefer_dark_theme(theme.name == "dark");
    }
    // 先摘再挂：切换主题时不让同一个 provider 被重复叠加。
    gtk::style_context_remove_provider_for_display(display, provider);
    provider.load_from_string(&theme.css());
    gtk::style_context_add_provider_for_display(
        display,
        provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}
