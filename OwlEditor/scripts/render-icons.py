#!/usr/bin/env python3
"""从 SVG 重新渲染 hicolor 各档 PNG 与一张对照预览图。

刻意使用 GdkPixbuf + librsvg：GNOME Shell / Ubuntu Dock 读取 .desktop 的
Icon= 时走的正是这条 SVG 渲染链。用别的渲染器（浏览器、Inkscape）导出，
容易出现「本地预览正常、dock 里显示不对」的偏差。

用法:
    python3 scripts/render-icons.py
输出:
    resources/icons/hicolor/<size>x<size>/apps/<APP_ID>.png
    docs/icon-preview-round5.png
"""
import os
import gi

gi.require_version("GdkPixbuf", "2.0")
from gi.repository import GdkPixbuf
from PIL import Image, ImageDraw

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
RES = os.path.join(ROOT, "resources")
DOCS = os.path.join(ROOT, "docs")
APP_ID = "io.github.owl.owleditor"
SIZES = [16, 24, 32, 48, 64, 128, 256, 512]
PREVIEW_SIZES = [16, 24, 32, 48, 64, 96]


def render(src: str, size: int, dst: str) -> None:
    os.makedirs(os.path.dirname(dst), exist_ok=True)
    pb = GdkPixbuf.Pixbuf.new_from_file_at_scale(src, size, size, True)
    pb.savev(dst, "png", [], [])


def preview() -> None:
    """深/浅两种 dock 底色上的实际显示效果，便于人工核对。"""
    w, h = 720, 300
    img = Image.new("RGB", (w, h), "#20242c")
    draw = ImageDraw.Draw(img)
    draw.rectangle((0, h // 2, w, h), fill="#eef1f5")
    x = 28
    for size in PREVIEW_SIZES:
        path = os.path.join(
            RES, "icons", "hicolor", f"{size}x{size}", "apps", APP_ID + ".png"
        )
        if not os.path.exists(path):
            # 预览尺寸不在正式档位时，从 SVG 临时渲染一次
            full = os.path.join(RES, APP_ID + ".svg")
            pb = GdkPixbuf.Pixbuf.new_from_file_at_scale(full, size, size, True)
            tmp = f"/tmp/owl-preview-{size}.png"
            pb.savev(tmp, "png", [], [])
            path = tmp
        icon = Image.open(path).convert("RGBA").resize(
            (size, size), Image.Resampling.LANCZOS
        )
        # 深色 dock 底
        img.paste(icon, (x, 56 - size // 2), icon)
        # 浅色 dock 底
        img.paste(icon, (x, h // 2 + 56 - size // 2), icon)
        draw.text((x, 108), f"{size}px", fill="#dfe5ee")
        draw.text((x, h // 2 + 108), f"{size}px", fill="#20242c")
        x += max(size, 44) + 62
    draw.text((28, 16), "OwlEditor dock icon  |  dark dock (top) / light dock (bottom)",
              fill="#ffffff")
    out = os.path.join(DOCS, "icon-preview-round6.png")
    os.makedirs(DOCS, exist_ok=True)
    img.save(out)
    print("  预览:", out)


def main() -> None:
    full = os.path.join(RES, APP_ID + ".svg")
    small = os.path.join(RES, APP_ID + "-small.svg")
    for size in SIZES:
        src = small if size <= 24 else full
        dst = os.path.join(
            RES, "icons", "hicolor", f"{size}x{size}", "apps", APP_ID + ".png"
        )
        render(src, size, dst)
        print(f"  {size:>3}x{size:<3} <- {os.path.basename(src)}")
    preview()


if __name__ == "__main__":
    main()
