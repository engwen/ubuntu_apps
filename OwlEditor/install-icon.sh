#!/usr/bin/env bash
# 把 OwlEditor 的 SVG、预渲染 PNG、.desktop 安装到用户级 hicolor 主题。
# 用法：./install-icon.sh [PREFIX]   默认 ~/.local/share
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PREFIX="${1:-$HOME/.local/share}"
APP_ID="io.github.owl.owleditor"

echo "安装前缀: $PREFIX"

# 可缩放 SVG
install -Dm644 "$ROOT/resources/$APP_ID.svg" \
  "$PREFIX/icons/hicolor/scalable/apps/$APP_ID.svg"
install -Dm644 "$ROOT/resources/$APP_ID-small.svg" \
  "$PREFIX/icons/hicolor/scalable/apps/$APP_ID-small.svg"

# 各档预渲染位图
for d in "$ROOT"/resources/icons/hicolor/*/apps; do
  [ -d "$d" ] || continue
  size="$(basename "$(dirname "$d")")"
  install -Dm644 "$d/$APP_ID.png" \
    "$PREFIX/icons/hicolor/$size/apps/$APP_ID.png"
done

# 桌面入口
install -Dm644 "$ROOT/resources/$APP_ID.desktop" \
  "$PREFIX/applications/$APP_ID.desktop"

if command -v gtk-update-icon-cache >/dev/null 2>&1; then
  gtk-update-icon-cache -f -t "$PREFIX/icons/hicolor" 2>/dev/null || true
fi
if command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database "$PREFIX/applications" 2>/dev/null || true
fi

echo "完成。图标名: $APP_ID"
