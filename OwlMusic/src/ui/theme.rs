//! 主题样式：所有颜色集中生成，切换主题/强调色只需重新 load 一次 CSS。
//!
//! 视觉基调参考了 mhertz 那套「深墨紫 + 浮起卡片 + 紫青渐变」的做法：
//! 窗口不是一整块纯色，而是深色底 + 两团很淡的紫/青光；内容装在圆角卡片里，
//! 卡片只有 1px 极淡的描边和一层柔和投影，不用重色块；强调色只出现在
//! 正在播放、进度条、主按钮这些「当前状态」上。

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
    /// 卡片底色：比面板实一点，用来做侧栏/播放条
    pub card: String,
    /// 窗口底上的两团柔光
    pub glow: String,
}

/// 线性混色，t 是 b 的比例
fn mix(a: (u8, u8, u8), b: (u8, u8, u8), t: f32) -> (u8, u8, u8) {
    let f = |x: u8, y: u8| ((x as f32) * (1.0 - t) + (y as f32) * t).round() as u8;
    (f(a.0, b.0), f(a.1, b.1), f(a.2, b.2))
}

/// 强调色的渐变色。
///
/// 蓝紫一类的强调色往青偏，做出参考站那种「紫 → 青」的科技感；
/// 暖色（黄、橙、红）往白提亮，免得黄→青变成脏绿。
pub fn accent_second(accent: (u8, u8, u8)) -> String {
    let (r, _, b) = accent;
    let cool = b >= r && b > 80;
    let (r2, g2, b2) = if cool {
        mix(accent, (0, 212, 255), 0.55)
    } else {
        mix(accent, (255, 255, 255), 0.34)
    };
    format!("#{r2:02X}{g2:02X}{b2:02X}")
}

pub fn palette(accent: &str, dark: bool) -> Palette {
    let c = parse_hex(accent);
    // 太暗的强调色在深色底上读不出来，抬一点亮度再用
    let c = if dark && (c.0 as u32 + c.1 as u32 + c.2 as u32) < 210 {
        mix(c, (255, 255, 255), 0.30)
    } else {
        c
    };
    let (r, g, b) = c;
    let accent = format!("#{r:02X}{g:02X}{b:02X}");
    let accent2 = accent_second(c);
    if dark {
        Palette {
            bg: "#06070D".into(),
            bg2: "#0E1120".into(),
            text: "#EAEEFF".into(),
            muted: "rgba(203,211,238,0.62)".into(),
            faint: "rgba(167,175,201,0.44)".into(),
            line: "rgba(255,255,255,0.075)".into(),
            panel: "rgba(255,255,255,0.05)".into(),
            hover: "rgba(255,255,255,0.07)".into(),
            accent: accent.clone(),
            accent2,
            accent_soft: format!("rgba({r},{g},{b},0.16)"),
            shadow: "rgba(0,0,0,0.55)".into(),
            card: "rgba(20,23,42,0.62)".into(),
            glow: format!(
                "radial-gradient(circle at 10% -6%, rgba({r},{g},{b},0.20), transparent 52%), \
                 radial-gradient(circle at 96% 104%, rgba(0,212,255,0.11), transparent 48%)"
            ),
        }
    } else {
        Palette {
            bg: "#F4F5FB".into(),
            bg2: "#FFFFFF".into(),
            text: "#141726".into(),
            muted: "rgba(20,23,38,0.60)".into(),
            faint: "rgba(20,23,38,0.40)".into(),
            line: "rgba(20,23,38,0.08)".into(),
            panel: "rgba(255,255,255,0.78)".into(),
            hover: "rgba(20,23,38,0.05)".into(),
            accent: accent.clone(),
            accent2,
            accent_soft: format!("rgba({r},{g},{b},0.13)"),
            shadow: "rgba(24,26,44,0.14)".into(),
            card: "rgba(255,255,255,0.80)".into(),
            glow: format!(
                "radial-gradient(circle at 8% -8%, rgba({r},{g},{b},0.14), transparent 50%), \
                 radial-gradient(circle at 98% 106%, rgba(0,212,255,0.10), transparent 46%)"
            ),
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
            "linear-gradient(to bottom, rgba({br},{bgc},{bb},0.74), rgba({br},{bgc},{bb},0.32) 18%, rgba({br},{bgc},{bb},0.26) 50%, rgba({br},{bgc},{bb},0.32) 82%, rgba({br},{bgc},{bb},0.80))"
        )
    } else {
        format!(
            "linear-gradient(to bottom, rgba({br},{bgc},{bb},0.84), rgba({br},{bgc},{bb},0.54) 18%, rgba({br},{bgc},{bb},0.46) 50%, rgba({br},{bgc},{bb},0.54) 82%, rgba({br},{bgc},{bb},0.88))"
        )
    };
    format!(
        r#"
window.owl,
.owl-root {{
  background-color: {bg};
  /* 两团很淡的柔光，让深色底不至于死板（reference: 深墨紫 + 冷青） */
  background-image: {glow};
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
.app-title {{ font-size: 15px; font-weight: 800; letter-spacing: 0.2px; }}
.app-sub {{ font-size: 11px; color: {muted}; }}

/* ---------- 左侧歌单：一张浮起的卡片 ---------- */
.sidebar {{
  background-color: {card};
  border: 1px solid {line};
  border-radius: 20px;
  /* 只留外边距，卡片之间靠留白分隔，不用分隔线 */
  margin: 6px 6px 10px 10px;
}}
.sidebar.right {{ margin: 6px 10px 10px 6px; }}
.side-head {{ padding: 12px 14px 6px 16px; }}
.side-title {{ font-size: 12px; font-weight: 800; letter-spacing: 1.4px; color: {muted}; }}
.side-count {{ font-size: 11px; color: {faint}; }}
.tracklist {{ background: transparent; }}
.tracklist > row {{
  border-radius: 12px;
  margin: 2px 8px;
  padding: 0;
  background-color: transparent;
  transition: background-color 140ms ease;
}}
.tracklist > row:hover {{ background-color: {hover}; }}
/* 列表的视觉状态只有一个：正在播放。GTK 的选中/焦点框一律不画——
   否则切歌后旧行还留着选中底色，"两个正在播放"的错觉就是这么来的。
   选中态依然存在于模型里（键盘导航、activate 定位都要用），只是不可见。
   拆成几条规则：万一某个伪类不被支持，不至于把整组选择器一起废掉。 */
.tracklist > row:selected,
.tracklist > row:selected:hover {{
  background-color: transparent;
  background-image: none;
  color: {text};
}}
.tracklist > row:focus {{
  background-color: transparent;
  background-image: none;
  outline-style: none;
  box-shadow: none;
}}
.tracklist > row:focus-visible {{
  outline-style: none;
  box-shadow: none;
}}
.track-row {{ padding: 8px 10px; }}
.track-idx {{ font-size: 11px; color: {faint}; min-width: 22px; }}
.track-title {{ font-size: 13.5px; font-weight: 600; }}
.track-sub {{ font-size: 11.5px; color: {muted}; }}
.track-dur {{ font-size: 11.5px; color: {faint}; }}
/* 正在播放：只换底色和文字色，不加边框/竖线之类的额外装饰。
   类的挂点是行内的 .track-row（列表的 row 节点拿不到），别写回 `row.playing`。 */
.track-row.playing {{
  background-color: {accent_soft};
  border-radius: 12px;
}}
.track-row.playing .track-title {{ color: {accent}; font-weight: 700; }}
.track-row.playing .track-idx {{ color: {accent}; }}
.track-row.playing .track-sub {{ color: {muted}; }}

/* ---------- 右侧舞台 ---------- */
/* 整栏都是歌词：不留内边距、不留圆角，上顶顶栏下顶播放条 */
.stage {{ padding: 0; }}
paned > separator {{
  min-width: 8px;
  background-color: transparent;
  transition: background-color 140ms ease;
}}
paned > separator:hover {{ background-color: {accent_soft}; }}
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
/* 歌词区：模糊封面只铺这一块，铺满整栏 */
.lyric-area {{ border-radius: 0; }}
.lyric-scrim {{ background-image: {scrim}; }}
/* 浮在歌词右上角的工具条：单独给一层底，压在歌词上也看得清 */
.lyric-tools {{
  background-color: {card};
  border: 1px solid {line};
  border-radius: 999px;
  padding: 2px 4px;
  box-shadow: 0 10px 26px {shadow};
}}
/* 滚动容器必须透明，否则背景图被它自己的底色挡住 */
.lyric-area scrolledwindow,
.lyric-area viewport {{
  background: transparent;
}}

/* ---------- 播放控制条：浮起的玻璃条 ---------- */
.playerbar {{
  background-color: {card};
  border: 1px solid {line};
  border-radius: 18px;
  /* 左右留白让它像一块浮在底部的卡片，高度仍由 ui::BAR_H 定死 */
  margin: 0 12px 10px 12px;
  min-height: 64px;
  padding: 6px 18px;
  box-shadow: 0 16px 40px {shadow};
}}
/* 小尺寸封面：52px 用 18px 圆角太大，单独收紧 */
.cover-frame.small {{
  border-radius: 12px;
  box-shadow: 0 6px 16px {shadow};
}}
.bar-title {{ font-size: 13.5px; font-weight: 700; }}
.bar-sub {{ font-size: 11.5px; color: {muted}; }}
.time {{ font-size: 11px; color: {faint}; min-width: 38px; }}

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
  box-shadow: 0 10px 26px {accent_soft};
}}
button.play-pill:hover {{ box-shadow: 0 12px 30px {accent_soft}; }}
button.ghost {{
  background-color: {panel}; color: {text};
  border: 1px solid {line}; border-radius: 10px;
  padding: 5px 12px; font-size: 12.5px; font-weight: 600;
  box-shadow: none; transition: background-color 140ms ease;
}}
button.ghost:hover {{ background-color: {hover}; }}
button.accent {{ background-image: linear-gradient(135deg, {accent}, {accent2}); color: #fff; border: none; }}

/* 进度：细轨 + 渐变填充，滑块平时藏起来，悬停才出现 */
scale.progress trough {{ min-height: 4px; border-radius: 999px; background-color: {panel}; }}
scale.progress highlight {{ border-radius: 999px; background-image: linear-gradient(90deg, {accent}, {accent2}); }}
scale.progress slider {{
  min-width: 11px; min-height: 11px; border-radius: 999px;
  background-color: #FFFFFF; box-shadow: 0 1px 4px rgba(0,0,0,0.45);
  opacity: 0; transition: opacity 120ms ease;
}}
scale.progress:hover slider, scale.progress:active slider {{ opacity: 1; }}
scale.vol trough {{ min-height: 3px; border-radius: 999px; background-color: {panel}; }}
scale.vol highlight {{ border-radius: 999px; background-color: {muted}; }}
scale.vol slider {{ min-width: 9px; min-height: 9px; border-radius: 999px; background-color: #FFFFFF; opacity: 0; }}
scale.vol:hover slider {{ opacity: 1; }}
scale {{ padding: 0; }}

/* 搜索框：参考站那种整颗胶囊，不是方块 */
entry.owl-search {{
  background-color: {panel};
  border: 1px solid {line};
  border-radius: 999px;
  min-height: 32px;
  color: {text};
  padding: 0 12px;
  transition: border-color 140ms ease, background-color 140ms ease;
}}
entry.owl-search:focus-within {{ border-color: {accent}; background-color: {hover}; }}
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

window.sheet {{ background-color: {bg}; color: {text}; background-image: {glow}; }}
.card {{
  background-color: {card};
  border: 1px solid {line};
  border-radius: 16px;
  padding: 14px;
}}
.matchlist > row {{ border-radius: 12px; margin: 2px 4px; padding: 4px; }}
.matchlist > row:hover {{ background-color: {hover}; }}
.matchlist > row:selected {{ background-color: {accent_soft}; }}
.match-title {{ font-size: 13.5px; font-weight: 700; }}
.match-sub {{ font-size: 11.5px; color: {muted}; }}
/* 歌词候选：逐行的那条给个实心标，纯文本保持淡一点 */
.badge-sync {{
  background-color: {accent};
  color: {bg};
  border-radius: 8px;
  padding: 3px 8px;
}}
/* 歌词预览：能读的正文，不要用 hint 那种灰 */
.lyric-peek {{ font-size: 12.5px; color: {text}; }}
.peek-box {{
  background-color: {card};
  border: 1px solid {line};
  border-radius: 14px;
}}
.hint {{ font-size: 11.5px; color: {faint}; }}
.tiny {{ font-size: 11px; color: {faint}; }}

/* 「按在线数据编辑标签」页顶部的来源说明 */
.net-banner {{
  background-color: {accent_soft};
  border: 1px solid {line};
  border-radius: 14px;
  padding: 10px 12px;
}}
.net-banner-title {{ font-size: 12.5px; font-weight: 700; color: {accent}; }}
/* 用网络值填进来、且和文件里不一样的字段：描一圈强调色 */
entry.from-net {{
  border-color: {accent};
}}

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
        card = p.card,
        glow = p.glow,
        scrim = scrim
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 蓝紫强调色要往青偏，暖色要往白提亮——黄→青会变脏绿
    #[test]
    fn accent_second_follows_hue_family() {
        let cool = super::parse_hex(&accent_second((124, 77, 255)));
        assert!(cool.2 == 255, "冷色渐变终点应当拉到青：{cool:?}");
        assert!(cool.1 > 140, "冷色渐变中间要偏青而不是偏紫：{cool:?}");

        let warm = super::parse_hex(&accent_second((249, 240, 107)));
        assert!(warm.0 >= 249 && warm.1 >= 240, "暖色应当被提亮：{warm:?}");
        assert!(warm.2 > 130, "暖色不应被拉成绿色：{warm:?}");
    }

    /// 深色主题下太暗的强调色要被抬起来，否则「正在播放」几乎看不见
    #[test]
    fn dark_palette_lifts_too_dark_accent() {
        let p = palette("#101010", true);
        let (r, g, b) = super::parse_hex(&p.accent);
        assert!(
            r as u32 + g as u32 + b as u32 > 210,
            "强调色没有被提亮：{}",
            p.accent
        );
    }

    /// 卡片和柔光都要在样式表里出现，缺一个「浮起卡片」的感觉就没了
    #[test]
    fn css_has_card_and_glow() {
        let c = css("#7C4DFF", true);
        assert!(
            c.contains("background-image: radial-gradient"),
            "缺窗口柔光"
        );
        assert!(c.contains(".playerbar"), "缺播放条样式");
        assert!(c.contains("border-radius: 20px"), "缺侧栏圆角");
    }
}
