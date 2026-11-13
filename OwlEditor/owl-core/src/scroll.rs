//! 滚动条 adjustment 的纯数值逻辑（不依赖 GTK，便于无显示后端时做回归验证）。
//!
//! 用户两次反馈「滚动条拖不动」，根因是同步代码每 40ms 无条件 `set_value`，
//! 把 GTK 正在进行的拖拽手势覆盖掉；另外 `upper - page_size` 的合法上界
//! 与状态端口径不一致，写进去的值被 GTK clamp，两边反复拉扯。
//! 这里把「算目标参数」和「该不该写」固化成可测的纯函数，窗口层只负责
//! 把它们接到 `GtkAdjustment` 上，测试则可以直接模拟完整事件序列。

/// 一次同步后 adjustment 应有的三个量。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Params {
    pub upper: f64,
    pub page: f64,
    pub value: f64,
}

/// 模拟 GTK `GtkAdjustment` 对 value 的约束：`[lower, upper - page_size]`。
/// 写入超范围的值会被 GTK clamp，状态与控件就会反复拉扯。
pub fn clamp_value(value: f64, upper: f64, page: f64) -> f64 {
    value.clamp(0.0, (upper - page).max(0.0))
}

/// 视口预取缓冲行数。
///
/// `visible_lines()` 返回 `floor(视口高/行高) + 2`：多出的 2 行是给
/// `refresh_view()` 预取数据、`scroll_by_display_rows()` 定位目标用的，
/// 绘制循环里 `y > 视口高` 就 break，所以**实际能完整画出来的只有
/// `visible_lines() - ROW_BUFFER` 行**。
///
/// 不换行时旧口径把 `max_top` 钉在 `N - visible_lines()`，视口显示
/// `[N-V, N-1]` 共 V 行，最后 ROW_BUFFER 行恰好落在窗口外 —— 用户反馈
/// 「取消自动换行后最下面又看不到了 / 滚到底最后一行被遮住」。
/// 上界必须多放 ROW_BUFFER 行，让最后一行能完整绘制。
pub const ROW_BUFFER: u64 = 2;

/// 垂直滚动条（不换行）：value 就是精确行号，`page_size` 是可见行数。
///
/// `upper` 比总行数多留 `ROW_BUFFER`，使 GTK 的合法上界
/// `upper - page_size` 正好等于 `max_top()`，状态端与控件端不再拉扯。
pub fn vertical(total_lines: u64, visible_lines: usize, top_line: u64) -> Params {
    let total = total_lines.max(1) as f64;
    let page = (visible_lines as f64).max(1.0);
    // upper 必须严格大于 page_size，否则 GTK 会把滚动条置成不可交互。
    let upper = (total + ROW_BUFFER as f64).max(page + 1.0);
    Params {
        upper,
        page,
        value: clamp_value(top_line as f64, upper, page),
    }
}

/// 状态端允许的最大 `top_line`，与 `vertical()` 的合法上界保持同一口径。
///
/// 允许越过 `N - V` 共 ROW_BUFFER 行，抵消预取缓冲，让文件最后一行
/// 能滚进实际可绘制区域；但不会超过 `N-1`（不产生一整屏空白）。
pub fn max_top(total_lines: u64, visible_lines: usize) -> u64 {
    let page = (visible_lines as u64).max(1);
    let n = total_lines.max(1);
    n.saturating_sub(page)
        .saturating_add(ROW_BUFFER)
        .min(n.saturating_sub(1))
}

/// 自动换行下，视口能容纳的「等效逻辑行数」。
///
/// 视口高度能放 `visible_rows` 个**显示行**；折行比为 `ratio` 时，
/// 这些显示行只对应 `visible_rows / ratio` 个逻辑行。滚动条 `page_size`
/// 必须用这个值，否则滑块比例与真实内容长度对不上。
pub fn wrapped_page(visible_rows: usize, wrap_ratio: f64) -> f64 {
    let ratio = wrap_ratio.clamp(1.0, 16.0);
    ((visible_rows as f64).max(1.0) / ratio).ceil().max(1.0)
}

/// 自动换行时的垂直滚动条参数。
///
/// 旧实现里 `upper = 逻辑行总数 N`、`page = 可见显示行数 V`、
/// `value = top_line`（逻辑行号）——三个量单位不一致，自动换行后出两个问题：
///
/// 1. `max_top = N - V` 让视口起点永远停在倒数第 V 条逻辑行。V 条逻辑行
///    展开成 M>V 个显示行后，屏幕只画得下前 V 个，**文件末尾的折行内容
///    永远滚不到**（用户反馈「滚动到底也看不到东西，最后一行被遮住」）。
/// 2. 滑块比例恒为 `V/N`，折行越多内容越长，滚动条却一点没变长
///    （用户反馈「滚动条计算长度有问题」）。
///
/// 现在把 `page` 折算成 `V/ratio`，并把 `top_line` 的上界放宽到 `N-1`
/// （允许滚动越过末尾，让最后一条逻辑行的全部折行都能显示）。
/// `value` 的整数部分是逻辑行号，小数部分编码 `top_sub`（该逻辑行内第几个
/// 显示行），这样拖动滚动条也能停在折行中间，不会一次跳过整个逻辑行。
pub fn vertical_wrapped(
    total_lines: u64,
    visible_rows: usize,
    wrap_ratio: f64,
    top_line: u64,
    top_sub: usize,
    sub_count: usize,
) -> Params {
    let ratio = wrap_ratio.clamp(1.0, 16.0);
    let page = wrapped_page(visible_rows, ratio);
    let max_top = total_lines.saturating_sub(1) as f64;
    // 必须给小数部分留余量：GTK 的 value 合法上界是 `upper - page_size`。
    // 若令 `upper - page = N-1`（整数），拖到底时 value 被钳成 N-1，
    // 小数部分（行内折行位置）整个丢掉，最后一行的后续折行又看不到了。
    // 多留 0.999999 后，滑块最底 value = max_top + 0.999999，反解出来
    // 正好是「最后一条逻辑行的最后一个显示行」。
    let upper = (max_top + 0.999_999 + page).max(page + 1.0);
    // 分母必须用「当前逻辑行实际折成的显示行数」，不能用平均折行比：
    // 单行超长时 top_sub 可能远大于 ratio，用 ratio 会让小数部分溢出被钳掉，
    // 拖动后视口位置回退（用户看到的「跳动」）。apply_rows 已把 top_sub
    // 钳到 sub_count-1，因此这里 frac 恒 < 1，value 可无损往返。
    let denom = sub_count.max(1) as f64;
    let frac = (top_sub as f64 / denom).min(0.999_999);
    Params {
        upper,
        page,
        value: clamp_value(top_line as f64 + frac, upper, page),
    }
}

/// 自动换行时 `top_line` 的合法上界，与 `vertical_wrapped()` 同口径。
pub fn max_top_wrapped(total_lines: u64) -> u64 {
    total_lines.saturating_sub(1)
}

/// 从滚动条 value 反解出 (逻辑行号, 行内显示行下标)。
/// `sub_count` 与 `vertical_wrapped()` 用同一个「当前行实际折行数」，
/// 否则小数部分的编码/解码口径不一致，拖动后会跳回别的折行位置。
pub fn top_from_value(value: f64, sub_count: usize) -> (u64, usize) {
    let denom = sub_count.max(1) as f64;
    let v = value.max(0.0);
    let line = v.floor() as u64;
    let sub = ((v - v.floor()) * denom).round() as usize;
    (line, sub.min(sub_count.saturating_sub(1)))
}

/// 在已展开的显示行序列里，从下标 `cur` 前进 `delta` 个显示行。
///
/// `logical[i]` 是第 i 个显示行所属逻辑行（相对当前 `top_line` 的下标），
/// `sub[i]` 是它在该逻辑行内的子行号。返回 `(逻辑行增量, 子行号, 溢出)`：
///
/// - 溢出 `= 0`：命中已加载区域，前两项就是新的视口起点；
/// - 溢出 `< 0`：往上越出已加载区域，绝对值是越过的显示行数；
/// - 溢出 `> 0`：往下越出已加载区域，值是越过的显示行数。
///
/// 自动换行下必须按显示行步进：按逻辑行滚一次会跳过整条逻辑行的所有
/// 折行。把它做成纯函数后，无显示后端也能重放完整的滚动序列。
pub fn step_display_rows(
    logical: &[usize],
    sub: &[usize],
    cur: usize,
    delta: i32,
) -> (u64, usize, i32) {
    let n = logical.len().min(sub.len());
    if n == 0 {
        return (0, 0, 0);
    }
    let t = cur as i64 + delta as i64;
    if t >= 0 && (t as usize) < n {
        let i = t as usize;
        (logical[i] as u64, sub[i], 0)
    } else if t < 0 {
        (0, 0, t as i32)
    } else {
        (0, 0, (t as usize - n + 1) as i32)
    }
}

/// 水平滚动条：按「打开以来见过的全局最长行」定范围。
///
/// 旧实现只看当前可见的几十行，滚到短行区域时 `upper` 会突然缩回 `page`，
/// `x_offset` 被硬 clamp 回 0，表现为「横向滚不动 / 突然弹回」。
/// `longest_px` 由调用方传入「视口最长行与历史最长行的较大者」。
pub fn horizontal(longest_px: f64, text_width: f64, x_offset: f64, wrap: bool) -> Params {
    let page = text_width.max(1.0);
    if wrap {
        return Params {
            upper: page,
            page,
            value: 0.0,
        };
    }
    let upper = longest_px.max(page);
    Params {
        upper,
        page,
        value: clamp_value(x_offset, upper, page),
    }
}

/// 只有值确实不同才允许写入 adjustment。
///
/// 用户拖动时 `value_changed` 已经把 `top_line` / `x_offset` 同步成控件当前值，
/// 于是这里返回 false —— 不会执行 `set_value`，GTK 的拖拽手势不会被中途打断。
pub fn needs_write(current: f64, target: f64, eps: f64) -> bool {
    (current - target).abs() > eps
}

/// 水平滚动时的小步跟随：只滚到刚好让光标露出，不整格跳跃。
/// 这是「编辑时文字跳动」的修复口径：返回新的 x_offset。
pub fn follow_cursor_px(
    cursor_px: f64,
    x_offset: f64,
    text_width: f64,
    margin: f64,
    upper: f64,
    page: f64,
) -> f64 {
    let margin = margin.max(0.0);
    let next = if cursor_px < x_offset + margin {
        (cursor_px - margin).max(0.0)
    } else if cursor_px > x_offset + text_width - margin {
        (cursor_px - text_width + margin).max(0.0)
    } else {
        x_offset
    };
    clamp_value(next, upper, page)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vertical_value_always_legal() {
        for total in [1u64, 2, 41, 100, 3001, 1_000_000] {
            for top in [0u64, 1, 40, total / 2, total, total + 10] {
                let p = vertical(total, 40, top);
                assert!(p.upper > p.page, "upper 必须大于 page_size");
                assert!(p.value >= 0.0 && p.value <= p.upper - p.page);
            }
        }
    }

    #[test]
    fn max_top_matches_vertical_upper() {
        for total in [1u64, 41, 100, 3001] {
            let mt = max_top(total, 40);
            let p = vertical(total, 40, mt);
            assert!((p.value - mt as f64).abs() < 1e-9);
            if total > 40 {
                assert!(
                    (p.upper - p.page - mt as f64).abs() < 1e-9,
                    "GTK 合法上界必须等于 max_top: {p:?}"
                );
            }
        }
    }

    #[test]
    fn unwrapped_bottom_really_shows_last_line() {
        // 视口 V 行里有 ROW_BUFFER 行只用于预取，实际只能画 V-2 行。
        // 旧口径 max_top = N-V，视口 [960, 999] 的最后 2 行落在窗口外，
        // 拖到底最后一行永远看不见。
        let (n, v) = (1000u64, 40usize);
        let drawable = v as u64 - ROW_BUFFER;
        let old_top = n - v as u64;
        assert!(old_top + drawable < n, "旧口径末尾确实被裁掉");
        let mt = max_top(n, v);
        assert_eq!(mt, n - drawable);
        // 从 mt 起画 drawable 行，最后一行 n-1 必须落在窗口内。
        assert!(mt + drawable >= n, "新口径末尾仍被裁掉");
        assert!(mt + drawable <= n + v as u64, "不能滚出一整屏空白");
        let p = vertical(n, v, mt);
        assert!((p.value - mt as f64).abs() < 1e-9);
        assert!((p.upper - p.page - mt as f64).abs() < 1e-9);
    }

    #[test]
    fn max_top_never_exceeds_last_line() {
        for total in [1u64, 2, 3, 40, 41, 42, 100, 3001] {
            for v in [4usize, 10, 40, 80] {
                let mt = max_top(total, v);
                assert!(mt < total, "max_top 越过文件末尾: {total}/{v}");
                let p = vertical(total, v, mt);
                assert!(p.value <= p.upper - p.page + 1e-9);
            }
        }
    }

    #[test]
    fn wrap_switch_clamps_top_to_new_limit() {
        // 换行模式允许 top_line 到 N-1；取消换行后上界收紧为 N-V+2，
        // hook 会把 top_line 钳回来，滚动条 value 也必须同步落在合法上界内。
        let (n, v) = (1000u64, 40usize);
        let from_wrapped = max_top_wrapped(n);
        assert_eq!(from_wrapped, 999);
        let unwrapped = max_top(n, v);
        let top = from_wrapped.min(unwrapped);
        assert_eq!(top, 962);
        let p = vertical(n, v, top);
        assert!((p.value - top as f64).abs() < 1e-9);
        assert!((p.upper - p.page - top as f64).abs() < 1e-9);
    }

    #[test]
    fn horizontal_range_never_shrinks_below_page() {
        let page = 900.0;
        let p = horizontal(2400.0, page, 500.0, false);
        assert!(p.upper > p.page);
        assert_eq!(p.value, 500.0);
        // 自动换行时没有水平滚动
        let w = horizontal(2400.0, page, 500.0, true);
        assert_eq!(w.upper, w.page);
        assert_eq!(w.value, 0.0);
    }

    #[test]
    fn dragged_value_is_never_rewritten() {
        // 用户把滚动条拖到 value=1234：value_changed 已把 top_line 同步为 1234，
        // 再算一次目标参数时，控件当前值 == 目标值，不应发生任何 set_value。
        let p = vertical(3001, 40, 1234);
        assert!(!needs_write(1234.0, p.value, 0.5));
    }

    #[test]
    fn wrapped_page_shrinks_as_lines_wrap_more() {
        assert_eq!(wrapped_page(40, 1.0), 40.0);
        assert_eq!(wrapped_page(40, 2.0), 20.0);
        assert_eq!(wrapped_page(40, 3.0), 14.0); // ceil(40/3)
        // 折行越严重，滑块越短（滚动条「变长」）。
        let short = vertical_wrapped(1000, 40, 1.0, 0, 0, 1);
        let long = vertical_wrapped(1000, 40, 3.0, 0, 0, 3);
        assert!(long.page < short.page);
        assert!(long.page / long.upper < short.page / short.upper);
    }

    #[test]
    fn wrapped_scroll_can_reach_last_logical_line() {
        // 旧口径 max_top = N - V = 960，末尾 40 条逻辑行的折行内容滚不到。
        let (n, v, ratio) = (1000u64, 40usize, 3.0);
        let max_top = max_top_wrapped(n);
        assert_eq!(max_top, 999);
        let p = vertical_wrapped(n, v, ratio, max_top, 0, 3);
        assert!((p.value - max_top as f64).abs() < 1e-9);
        assert!(p.upper > p.page);
        assert!(p.value <= p.upper - p.page + 1e-9);
    }

    #[test]
    fn wrapped_value_encodes_sub_and_roundtrips() {
        let (ratio, sub_count) = (3.0, 3usize);
        for (line, sub) in [(0u64, 0usize), (10, 1), (10, 2), (999, 0), (250, 2)] {
            let p = vertical_wrapped(1000, 40, ratio, line, sub, sub_count);
            let (l2, s2) = top_from_value(p.value, sub_count);
            assert_eq!(l2, line, "行号往返不一致");
            assert_eq!(s2, sub, "行内显示行往返不一致");
        }
    }

    #[test]
    fn last_line_last_sub_row_is_reachable() {
        // 最后一条逻辑行折成 3 个显示行，滚到底必须能停在第 3 个（sub=2），
        // 否则最后两个显示行仍会被 value 上界钳掉。
        let (n, v, ratio, sub_count) = (1000u64, 40usize, 3.0, 3usize);
        let last = max_top_wrapped(n);
        let p = vertical_wrapped(n, v, ratio, last, 2, sub_count);
        assert!(p.value > last as f64 + 0.6, "小数部分被钳掉了: {:?}", p);
        assert_eq!(top_from_value(p.value, sub_count), (last, 2));
        // 滑块拖到合法最底点，也必须落在最后一个显示行。
        let bottom = top_from_value(p.upper - p.page, sub_count);
        assert_eq!(bottom, (last, 2), "滑块最底点没落在文件末尾");
    }

    #[test]
    fn very_long_single_line_sub_does_not_overflow_value() {
        // 平均折行比只有 2，但当前这一行折了 60 个显示行（top_sub 最大 59）。
        // 旧实现用 ratio 当分母会把小数部分钳掉，拖动后视口跳回 sub=2。
        let (ratio, sub_count) = (2.0, 60usize);
        for sub in [0usize, 1, 17, 30, 59] {
            let p = vertical_wrapped(1000, 40, ratio, 100, sub, sub_count);
            let (line2, sub2) = top_from_value(p.value, sub_count);
            assert_eq!(line2, 100);
            assert_eq!(sub2, sub, "超长行的折行位置必须无损往返");
        }
    }

    #[test]
    fn wrapped_value_is_always_legal() {
        for total in [1u64, 2, 41, 100, 3001] {
            for ratio in [1.0f64, 1.5, 3.0, 8.0] {
                for top in [0u64, 1, total / 2, total.saturating_sub(1), total + 5] {
                    for sub in [0usize, 1, 7, 255] {
                        // sub_count 恒大于 sub，模拟「当前行折了很多显示行」。
                        let p = vertical_wrapped(total, 40, ratio, top, sub, sub + 1);
                        assert!(p.upper > p.page, "upper 必须大于 page_size");
                        assert!(p.value >= 0.0);
                        assert!(
                            p.value <= p.upper - p.page + 1e-6,
                            "value 越过 GTK 合法上界: {:?}",
                            p
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn step_display_rows_walks_into_wrapped_rows() {
        // 3 条逻辑行，折成 6 个显示行：行0→3 个子行，行1→2 个，行2→1 个
        let logical = vec![0usize, 0, 0, 1, 1, 2];
        let sub = vec![0usize, 1, 2, 0, 1, 0];
        // 在同一条逻辑行内部前进：不动 top_line，只动 top_sub
        assert_eq!(step_display_rows(&logical, &sub, 0, 1), (0, 1, 0));
        assert_eq!(step_display_rows(&logical, &sub, 1, 1), (0, 2, 0));
        // 跨到下一条逻辑行
        assert_eq!(step_display_rows(&logical, &sub, 2, 1), (1, 0, 0));
        assert_eq!(step_display_rows(&logical, &sub, 3, 1), (1, 1, 0));
        assert_eq!(step_display_rows(&logical, &sub, 4, 1), (2, 0, 0));
        // 越界方向
        assert_eq!(step_display_rows(&logical, &sub, 0, -1).2, -1);
        assert_eq!(step_display_rows(&logical, &sub, 5, 1).2, 1);
        assert_eq!(step_display_rows(&logical, &sub, 5, 4).2, 4);
    }

    #[test]
    fn follow_cursor_moves_in_small_steps() {
        let (upper, page) = (4000.0, 900.0);
        let mut x = 0.0;
        let mut prev = x;
        let mut max_step = 0.0f64;
        // 逐字输入，光标每步前进一个字符宽（9px）
        for col in 1..400 {
            let px = col as f64 * 9.0;
            x = follow_cursor_px(px, x, page, 18.0, upper, page);
            max_step = max_step.max((x - prev).abs());
            prev = x;
        }
        // 旧实现一次跳 8 个字符宽 = 72px；现在单步不超过一个字符宽 + 余量。
        assert!(max_step <= 20.0, "单步水平跳动过大: {max_step}");
    }
}
