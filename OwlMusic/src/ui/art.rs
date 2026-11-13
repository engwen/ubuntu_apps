//! 封面处理：EXIF 无关的纯像素处理——圆角遮罩、极小尺寸高斯近似模糊、主色提取。
//! 全部在 CPU 完成，不依赖任何图像解码库（纹理来自 GDK）。

use gtk::gdk;
use gtk::gdk::prelude::*;
use gtk::glib;

pub struct Artwork {
    /// 圆角后的封面纹理
    pub cover: gdk::Texture,
    /// 供背景使用的小尺寸模糊纹理
    pub blur: gdk::Texture,
    /// 主色
    pub accent: (u8, u8, u8),
}

pub fn from_bytes(bytes: &[u8], blur_strength: f32) -> Option<Artwork> {
    let gbytes = glib::Bytes::from(bytes);
    let tex = gdk::Texture::from_bytes(&gbytes).ok()?;
    let w = tex.width();
    let h = tex.height();
    if w <= 0 || h <= 0 {
        return None;
    }
    let dl = gdk::TextureDownloader::new(&tex);
    let (data, stride) = dl.download_bytes();
    let raw = data.as_ref();
    let row_bytes = (w as usize) * 4;
    if raw.len() < row_bytes * (h as usize) {
        return None;
    }
    let mut rgba = vec![0u8; row_bytes * h as usize];
    for y in 0..h as usize {
        let s = y * stride;
        let d = y * row_bytes;
        rgba[d..d + row_bytes].copy_from_slice(&raw[s..s + row_bytes]);
    }

    // 主色：饱和度高、亮度适中的像素加权平均
    let accent = dominant_color(&rgba, w as usize, h as usize);

    // 背景：先降采样再模糊。
    // 之前用 40×40，铺到歌词区放大十几倍会有明显色块；128×128 仍然是「糊」，
    // 但颗粒更细，看起来才像毛玻璃而不是马赛克。
    let (bw, bh) = (128usize, 128usize);
    let mut small = downsample(&rgba, w as usize, h as usize, bw, bh);
    let k = blur_strength.clamp(0.0, 2.0);
    box_blur(&mut small, bw, bh, (10.0 * k).round().max(1.0) as usize);
    box_blur(&mut small, bw, bh, (6.0 * k).round().max(1.0) as usize);
    let blur_bytes = glib::Bytes::from_owned(small);
    let blur = gdk::MemoryTexture::new(
        bw as i32,
        bh as i32,
        gdk::MemoryFormat::R8g8b8a8,
        &blur_bytes,
        bw * 4,
    );

    // 封面圆角
    let radius = (w.min(h) as f32 * 0.055).clamp(8.0, 40.0);
    round_corners(&mut rgba, w as usize, h as usize, radius);
    let cover_bytes = glib::Bytes::from_owned(rgba);
    let cover = gdk::MemoryTexture::new(
        w,
        h,
        gdk::MemoryFormat::R8g8b8a8,
        &cover_bytes,
        w as usize * 4,
    );

    Some(Artwork {
        cover: cover.upcast(),
        blur: blur.upcast(),
        accent,
    })
}

/// 把任意尺寸的封面裁成正方形并缩到 `px`×`px`。
///
/// 为什么要这一步：`gtk::Picture` 的 natural size 取的是纹理自身的尺寸，
/// `set_size_request()` 只是**最小**尺寸。直接把 1000×1000 的原图塞进 52px 的
/// 容器，布局仍会按 1000 算，播放条就被专辑图撑高了。所以这里给出真正的定尺纹理。
pub fn square(src: &gdk::Texture, px: u32) -> Option<gdk::Texture> {
    let (w, h) = (src.width(), src.height());
    if w <= 0 || h <= 0 || px == 0 {
        return None;
    }
    let dl = gdk::TextureDownloader::new(src);
    let (data, stride) = dl.download_bytes();
    let raw = data.as_ref();
    if raw.len() < stride * (h as usize - 1) + (w as usize) * 4 {
        return None;
    }

    // 中心裁正方形，避免封面被拉变形
    let side = w.min(h) as usize;
    let ox = ((w - side as i32) / 2) as usize;
    let oy = ((h - side as i32) / 2) as usize;

    let n = px as usize;
    let mut out = vec![0u8; n * n * 4];
    for by in 0..n {
        let y0 = oy + by * side / n;
        let y1 = (oy + ((by + 1) * side / n)).max(y0 + 1).min(oy + side);
        for bx in 0..n {
            let x0 = ox + bx * side / n;
            let x1 = (ox + ((bx + 1) * side / n)).max(x0 + 1).min(ox + side);
            let (mut r, mut g, mut b, mut a, mut cnt) = (0u32, 0u32, 0u32, 0u32, 0u32);
            for y in y0..y1 {
                let s = y * stride;
                for x in x0..x1 {
                    let i = s + x * 4;
                    if i + 3 < raw.len() {
                        r += raw[i] as u32;
                        g += raw[i + 1] as u32;
                        b += raw[i + 2] as u32;
                        a += raw[i + 3] as u32;
                        cnt += 1;
                    }
                }
            }
            let cnt = cnt.max(1);
            let o = (by * n + bx) * 4;
            out[o] = (r / cnt) as u8;
            out[o + 1] = (g / cnt) as u8;
            out[o + 2] = (b / cnt) as u8;
            out[o + 3] = (a / cnt) as u8;
        }
    }

    let bytes = glib::Bytes::from_owned(out);
    let tex = gdk::MemoryTexture::new(
        px as i32,
        px as i32,
        gdk::MemoryFormat::R8g8b8a8,
        &bytes,
        n * 4,
    );
    Some(tex.upcast())
}

fn round_corners(rgba: &mut [u8], w: usize, h: usize, r: f32) {
    let r = r.min(w as f32 / 2.0).min(h as f32 / 2.0);
    let r2 = r * r;
    for y in 0..h {
        for x in 0..w {
            let corner = if (x as f32) < r && (y as f32) < r {
                Some((r, r))
            } else if (x as f32) >= w as f32 - r && (y as f32) < r {
                Some((w as f32 - r, r))
            } else if (x as f32) < r && (y as f32) >= h as f32 - r {
                Some((r, h as f32 - r))
            } else if (x as f32) >= w as f32 - r && (y as f32) >= h as f32 - r {
                Some((w as f32 - r, h as f32 - r))
            } else {
                None
            };
            if let Some((cx, cy)) = corner {
                let dx = x as f32 + 0.5 - cx;
                let dy = y as f32 + 0.5 - cy;
                let d2 = dx * dx + dy * dy;
                if d2 > r2 {
                    let i = (y * w + x) * 4 + 3;
                    rgba[i] = 0;
                } else {
                    // 抗锯齿边缘
                    let d = d2.sqrt();
                    let edge = r - d;
                    if edge < 1.0 {
                        let i = (y * w + x) * 4 + 3;
                        let a = (rgba[i] as f32 * edge).clamp(0.0, 255.0);
                        rgba[i] = a as u8;
                    }
                }
            }
        }
    }
}

fn downsample(src: &[u8], w: usize, h: usize, bw: usize, bh: usize) -> Vec<u8> {
    let mut out = vec![0u8; bw * bh * 4];
    for by in 0..bh {
        for bx in 0..bw {
            let x0 = bx * w / bw;
            let x1 = (((bx + 1) * w) / bw).max(x0 + 1).min(w);
            let y0 = by * h / bh;
            let y1 = (((by + 1) * h) / bh).max(y0 + 1).min(h);
            let (mut r, mut g, mut b, mut a, mut n) = (0u32, 0u32, 0u32, 0u32, 0u32);
            for y in y0..y1 {
                for x in x0..x1 {
                    let i = (y * w + x) * 4;
                    r += src[i] as u32;
                    g += src[i + 1] as u32;
                    b += src[i + 2] as u32;
                    a += src[i + 3] as u32;
                    n += 1;
                }
            }
            let n = n.max(1);
            let o = (by * bw + bx) * 4;
            out[o] = (r / n) as u8;
            out[o + 1] = (g / n) as u8;
            out[o + 2] = (b / n) as u8;
            out[o + 3] = (a / n) as u8;
        }
    }
    out
}

fn box_blur(buf: &mut [u8], w: usize, h: usize, radius: usize) {
    let src = buf.to_vec();
    let r = radius as isize;
    for y in 0..h {
        for x in 0..w {
            let mut acc = [0u32; 3];
            let mut n = 0u32;
            for dy in -r..=r {
                for dx in -r..=r {
                    let ny = y as isize + dy;
                    let nx = x as isize + dx;
                    if ny < 0 || nx < 0 || ny >= h as isize || nx >= w as isize {
                        continue;
                    }
                    let i = (ny as usize * w + nx as usize) * 4;
                    acc[0] += src[i] as u32;
                    acc[1] += src[i + 1] as u32;
                    acc[2] += src[i + 2] as u32;
                    n += 1;
                }
            }
            let o = (y * w + x) * 4;
            buf[o] = (acc[0] / n) as u8;
            buf[o + 1] = (acc[1] / n) as u8;
            buf[o + 2] = (acc[2] / n) as u8;
            buf[o + 3] = 255;
        }
    }
}

fn dominant_color(rgba: &[u8], w: usize, h: usize) -> (u8, u8, u8) {
    let step = (w * h / 4096).max(1);
    let (mut rr, mut gg, mut bb, mut wt) = (0f32, 0f32, 0f32, 0f32);
    let mut idx = 0usize;
    while idx < w * h {
        let i = idx * 4;
        if i + 3 < rgba.len() {
            let r = rgba[i] as f32 / 255.0;
            let g = rgba[i + 1] as f32 / 255.0;
            let b = rgba[i + 2] as f32 / 255.0;
            if rgba[i + 3] > 128 {
                let mx = r.max(g).max(b);
                let mn = r.min(g).min(b);
                let sat = mx - mn;
                let light = (mx + mn) / 2.0;
                // 偏好中等亮度、有饱和度的颜色
                let weight = (0.15 + sat * 1.6) * (1.0 - (light - 0.5).abs() * 0.9).max(0.1);
                rr += r * weight;
                gg += g * weight;
                bb += b * weight;
                wt += weight;
            }
        }
        idx += step;
    }
    if wt <= 0.0 {
        return (139, 124, 246);
    }
    let mut r = rr / wt;
    let mut g = gg / wt;
    let mut b = bb / wt;
    // 提亮/提饱和，保证作为强调色足够醒目
    let l = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    if l < 0.45 {
        let k = (0.52 / l.max(0.05)).min(2.4);
        r = (r * k).min(1.0);
        g = (g * k).min(1.0);
        b = (b * k).min(1.0);
    }
    let mix = 0.18;
    (
        ((r * (1.0 - mix) + 0.55 * mix) * 255.0) as u8,
        ((g * (1.0 - mix) + 0.58 * mix) * 255.0) as u8,
        ((b * (1.0 - mix) + 0.72 * mix) * 255.0) as u8,
    )
}

pub fn hex(c: (u8, u8, u8)) -> String {
    format!("#{:02X}{:02X}{:02X}", c.0, c.1, c.2)
}

pub fn parse_hex(s: &str) -> (u8, u8, u8) {
    let t = s.trim().trim_start_matches('#');
    if t.len() >= 6 {
        let r = u8::from_str_radix(&t[0..2], 16).unwrap_or(139);
        let g = u8::from_str_radix(&t[2..4], 16).unwrap_or(124);
        let b = u8::from_str_radix(&t[4..6], 16).unwrap_or(246);
        (r, g, b)
    } else {
        (139, 124, 246)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gtk::prelude::*;

    /// 造一张左红右蓝的纯色纹理
    fn solid(w: i32, h: i32) -> gdk::Texture {
        let mut px = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                if x < w / 2 {
                    px[i] = 255;
                } else {
                    px[i + 2] = 255;
                }
                px[i + 3] = 255;
            }
        }
        let b = glib::Bytes::from_owned(px);
        gdk::MemoryTexture::new(w, h, gdk::MemoryFormat::R8g8b8a8, &b, (w * 4) as usize).upcast()
    }

    /// 播放条/信息头的尺寸完全由纹理决定，所以缩略图必须精确是目标尺寸，
    /// 否则 1000px 的原图会把固定高度的底栏顶高。
    #[test]
    fn square_is_exactly_sized() {
        let t = solid(200, 120);
        for px in [52u32, 168] {
            let s = square(&t, px).expect("应当能缩放");
            assert_eq!((s.width(), s.height()), (px as i32, px as i32));
        }
    }

    /// 这条测试说明「为什么必须缩放」：容器限死 52px 也没用，
    /// `gtk::Picture` 的 natural size 跟着纹理走，400px 的原图会报 400。
    /// 依赖 display，默认忽略，用 `--ignored` 手动跑。
    #[test]
    #[ignore]
    fn picture_natural_size_follows_texture() {
        if gtk::init().is_err() {
            println!("跳过：没有可用的 display");
            return;
        }
        let t = solid(400, 400);
        let pic = gtk::Picture::new();
        pic.set_content_fit(gtk::ContentFit::Cover);
        pic.set_size_request(52, 52);

        pic.set_paintable(Some(&t));
        let (min_raw, nat_raw, _, _) = pic.measure(gtk::Orientation::Vertical, -1);
        println!("原图：min={min_raw} natural={nat_raw}");

        let thumb = square(&t, 52).unwrap();
        pic.set_paintable(Some(&thumb));
        let (min_s, nat_s, _, _) = pic.measure(gtk::Orientation::Vertical, -1);
        println!("缩略图：min={min_s} natural={nat_s}");

        assert!(nat_raw >= 400, "原图的 natural 应当撑到 400");
        assert_eq!(nat_s, 52, "缩略图必须稳定在 52");
    }

    #[test]
    fn square_center_crops_instead_of_stretching() {
        let t = solid(200, 120);
        let s = square(&t, 20).unwrap();
        let dl = gdk::TextureDownloader::new(&s);
        let (data, stride) = dl.download_bytes();
        let raw = data.as_ref();
        let rgb = |x: usize, y: usize| {
            let i = y * stride + x * 4;
            (raw[i], raw[i + 1], raw[i + 2])
        };
        // 中心裁出 120x120（x 40..160），左半红右半蓝，不该出现拉伸渐变
        assert!(rgb(2, 10).0 > 200 && rgb(2, 10).2 < 40, "左侧应当是红色");
        assert!(rgb(17, 10).2 > 200 && rgb(17, 10).0 < 40, "右侧应当是蓝色");
    }
}
