//! 主题样式：所有颜色集中生成，切换主题/强调色只需重新 load 一次 CSS。

use super::art::parse_hex;

pub struct Palette {
    pub bg: String,
    pub bg2: String,
    pub text: String,
    pub muted: String,
    pub faint: String,
    pub line: String,
    pub panel: String,
    pub hover: String,
    pub accent: String,
    pub accent2: String,
    pub accent_soft: String,
    pub shadow: String,
}

pub fn palette(accent: &str, dark: bool) -> Palette {
    let (r, g, b) = parse_hex(accent);
    let accent = format!("#{r:02X}{g:02X}{b:02X}");
    let accent2 = format!(
        "#{:02X}{:02X}{:02X}",
        (r as u16 * 6 / 10 + 255 * 4 / 10).min(255),
        (g as u16 * 55 / 100 + 255 * 45 / 100).min(255),
        (b as u16 * 5 / 10 + 255 * 5 / 10).min(255)
    );
    if dark {
        Palette {
            bg: "#0B0C10".into(),
            bg2: "#14161C".into(),
            text: "#EDEEF3".into(),
            muted: "rgba(237,238,243,0.58)".into(),
            faint: "rgba(237,238,243,0.36)".into(),
            line: "rgba(255,255,255,0.09)".into(),
            panel: "rgba(255,255,255,0.045)".into(),
            hover: "rgba(255,255,255,0.075)".into(),
            accent: accent.clone(),
            accent2,
            accent_soft: format!("rgba({r},{g},{b},0.18)"),
            shadow: "rgba(0,0,0,0.55)".into(),
        }
    } else {
        Palette {
            bg: "#F2F3F7".into(),
            bg2: "#FFFFFF".into(),
            text: "#15171C".into(),
            muted: "rgba(21,23,28,0.60)".into(),
            faint: "rgba(21,23,28,0.38)".into(),
            line: "rgba(0,0,0,0.09)".into(),
            panel: "rgba(255,255,255,0.72)".into(),
            hover: "rgba(0,0,0,0.055)".into(),
            accent: accent.clone(),
            accent2,
            accent_soft: format!("rgba({r},{g},{b},0.15)"),
            shadow: "rgba(20,22,30,0.18)".into(),
        }
    }
}

pub fn css(accent: &str, dark: bool) -> String {
    let p = palette(accent, dark);
    // 歌词区背景的压暗层：上下重、中间轻，保证首尾两行也可读，
    // 同时不全遮住封面，能看出是「毛玻璃」而不是一块纯色板。
    let (br, bgc, bb) = parse_hex(&p.bg);
    let scrim = if dark {
        format!(
            "linear-gradient(to bottom, rgba({br},{bgc},{bb},0.72), rgba({br},{bgc},{bb},0.34) 16%, rgba({br},{bgc},{bb},0.28) 50%, rgba({br},{bgc},{bb},0.34) 84%, rgba({br},{bgc},{bb},0.78))"
        )
    } else {
        format!(
            "linear-gradient(to bottom, rgba({br},{bgc},{bb},0.82), rgba({br},{bgc},{bb},0.52) 16%, rgba({br},{bgc},{bb},0.46) 50%, rgba({br},{bgc},{bb},0.52) 84%, rgba({br},{bgc},{bb},0.86))"
        )
    };
    format!(
        r#"
window.owl {{
  background-color: {bg};
  color: {text};
  font-family: "Noto Sans CJK SC", "Source Han Sans SC", "Ubuntu", sans-serif;
}}
headerbar.owl-header {{
  background: transparent;
  box-shadow: none;
  border: none;
  /* 与 ui::HEADER_H 保持一致：顶栏固定 54px（46 内容 + 上下 4 padding） */
  min-height: 46px;
  padding: 4px 12px;
}}
headerbar.owl-header windowcontrols button {{
  min-width: 24px; min-height: 24px;
  border-radius: 999px;
}}
.app-title {{ font-size: 15px; font-weight: 800; letter-spacing: 0.3px; }}
.app-sub {{ font-size: 11px; color: {muted}; }}

/* ---------- 左侧歌单 ---------- */
.sidebar {{ border-right: 1px solid {line}; background-color: transparent; }}
.sidebar.right {{ border-right: none; border-left: 1px solid {line}; }}
.side-head {{ padding: 6px 16px 8px 18px; }}
.side-title {{ font-size: 13px; font-weight: 800; letter-spacing: 1.2px; color: {muted}; }}
.side-count {{ font-size: 11px; color: {faint}; }}
.tracklist {{ background: transparent; }}
.tracklist > row {{
  border-radius: 12px;
  margin: 1px 8px;
  padding: 0;
  background-color: transparent;
  transition: background-color 140ms ease;
}}
.tracklist > row:hover {{ background-color: {hover}; }}
.tracklist > row:selected {{ background-color: {accent_soft}; }}
.tracklist > row:selected .track-title {{ color: {accent}; }}
.track-row {{ padding: 7px 10px; }}
.track-idx {{ font-size: 11px; color: {faint}; min-width: 20px; }}
.track-title {{ font-size: 14px; font-weight: 600; }}
.track-sub {{ font-size: 11.5px; color: {muted}; }}
.track-dur {{ font-size: 11.5px; color: {faint}; }}
/* 正在播放：整行浅着色 + 左侧一道强调色竖线，未选中时也能一眼认出来。
   类的挂点是行内的 .track-row（列表的 row 节点拿不到），别写回 `row.playing`。 */
.track-row.playing {{
  background-color: {accent_soft};
  border-radius: 12px;
  box-shadow: inset 3px 0 0 {accent};
}}
/* 选中行本身已有底色，行内不再叠一层，只留强调色竖线 */
.tracklist > row:selected .track-row {{ background-color: transparent; }}
.track-row.playing .track-title {{ color: {accent}; }}
.track-row.playing .track-idx {{ color: {accent}; }}
.track-row.playing .track-sub {{ color: {muted}; }}

/* ---------- 右侧舞台 ---------- */
.stage {{ padding: 4px 26px 0 30px; }}
.np-title {{ font-size: 29px; font-weight: 800; letter-spacing: -0.2px; }}
.np-artist {{ font-size: 15px; color: {muted}; }}
.np-sub {{ font-size: 12px; color: {faint}; }}
.cover-frame {{
  border-radius: 18px;
  box-shadow: 0 20px 44px {shadow};
  background-color: {panel};
}}
.badge {{
  font-size: 10.5px; font-weight: 700;
  padding: 2px 9px; border-radius: 999px;
  background-color: {accent_soft};
  color: {accent};
}}
/* 歌词区：模糊封面只铺这一块，圆角 + 裁剪 */
.lyric-area {{ border-radius: 20px; }}
.lyric-scrim {{
  border-radius: 20px;
  background-image: {scrim};
}}
/* 滚动容器必须透明，否则背景图被它自己的底色挡住 */
.lyric-area scrolledwindow,
.lyric-area viewport {{
  background: transparent;
}}

/* ---------- 播放控制条（固定高度，与 ui::BAR_H 对应） ---------- */
.playerbar {{
  background-color: {bg2};
  border-top: 1px solid {line};
  min-height: 64px;
  padding: 6px 20px;
}}
/* 小尺寸封面：52px 用 18px 圆角太大，单独收紧 */
.cover-frame.small {{
  border-radius: 12px;
  box-shadow: 0 6px 16px {shadow};
}}
.bar-title {{ font-size: 13.5px; font-weight: 700; }}
.bar-sub {{ font-size: 11.5px; color: {muted}; }}
.time {{ font-size: 11.5px; color: {faint}; min-width: 40px; }}

button.flat {{
  background: transparent; border: none; box-shadow: none;
  color: {muted}; border-radius: 999px;
  min-width: 34px; min-height: 34px; padding: 4px;
  transition: background-color 140ms ease, color 140ms ease;
}}
button.flat:hover {{ background-color: {hover}; color: {text}; }}
button.flat:checked {{ color: {accent}; background-color: {accent_soft}; }}
button.play-pill {{
  background-image: linear-gradient(135deg, {accent}, {accent2});
  color: #FFFFFF; border: none;
  min-width: 46px; min-height: 46px; border-radius: 999px;
  box-shadow: 0 8px 22px {accent_soft};
}}
button.play-pill:hover {{ box-shadow: 0 10px 26px {accent_soft}; }}
button.ghost {{
  background-color: {panel}; color: {text};
  border: 1px solid {line}; border-radius: 10px;
  padding: 5px 12px; font-size: 12.5px; font-weight: 600;
  box-shadow: none; transition: background-color 140ms ease;
}}
button.ghost:hover {{ background-color: {hover}; }}
button.accent {{ background-image: linear-gradient(135deg, {accent}, {accent2}); color: #fff; border: none; }}
button.accent:hover {{ }}

scale.progress trough {{ min-height: 4px; border-radius: 999px; background-color: {panel}; }}
scale.progress highlight {{ border-radius: 999px; background-image: linear-gradient(90deg, {accent}, {accent2}); }}
scale.progress slider {{
  min-width: 11px; min-height: 11px; border-radius: 999px;
  background-color: #FFFFFF; box-shadow: 0 1px 4px rgba(0,0,0,0.35);
  opacity: 0; transition: opacity 120ms ease;
}}
scale.progress:hover slider, scale.progress:active slider {{ opacity: 1; }}
scale.vol trough {{ min-height: 3px; border-radius: 999px; background-color: {panel}; }}
scale.vol highlight {{ border-radius: 999px; background-color: {text}; }}
scale.vol slider {{ min-width: 9px; min-height: 9px; border-radius: 999px; background-color: #FFFFFF; opacity: 0; }}
scale.vol:hover slider {{ opacity: 1; }}
scale {{ padding: 0; }}

entry.owl-search {{
  background-color: {panel};
  border: 1px solid {line};
  border-radius: 10px;
  min-height: 30px;
  color: {text};
  padding: 0 8px;
  transition: border-color 140ms ease;
}}
entry.owl-search:focus-within {{ border-color: {accent}; }}
entry.owl-search > text {{ background: transparent; color: {text}; }}
entry.owl-search image {{ color: {faint}; }}

popover.owl-pop > contents {{
  background-color: {bg2};
  border: 1px solid {line};
  border-radius: 16px;
  padding: 14px;
  box-shadow: 0 24px 60px {shadow};
}}
.sheet-title {{ font-size: 12px; font-weight: 800; color: {muted}; letter-spacing: 0.8px; }}
.field-label {{ font-size: 12.5px; color: {muted}; }}
.value-label {{ font-size: 12px; color: {faint}; min-width: 38px; }}

window.sheet {{ background-color: {bg}; color: {text}; }}
.card {{
  background-color: {panel};
  border: 1px solid {line};
  border-radius: 16px;
  padding: 14px;
}}
.matchlist > row {{ border-radius: 12px; margin: 2px 4px; padding: 4px; }}
.matchlist > row:hover {{ background-color: {hover}; }}
.matchlist > row:selected {{ background-color: {accent_soft}; }}
.match-title {{ font-size: 13.5px; font-weight: 700; }}
.match-sub {{ font-size: 11.5px; color: {muted}; }}
.hint {{ font-size: 11.5px; color: {faint}; }}
.tiny {{ font-size: 11px; color: {faint}; }}

switch.owl-switch {{ border-radius: 999px; }}
button.swatch {{
  min-width: 22px; min-height: 22px; border-radius: 999px;
  border: 2px solid transparent; padding: 0;
}}
button.swatch.sel {{ border-color: {text}; }}
separator {{ background-color: {line}; min-width: 1px; min-height: 1px; }}
"#,
        bg = p.bg,
        bg2 = p.bg2,
        text = p.text,
        muted = p.muted,
        faint = p.faint,
        line = p.line,
        hover = p.hover,
        accent = p.accent,
        accent2 = p.accent2,
        accent_soft = p.accent_soft,
        panel = p.panel,
        shadow = p.shadow,
        scrim = scrim
    )
}
