#!/usr/bin/env bash
# 只装图标 / 桌面条目，不动二进制。开发时直接跑 target/debug/owlmusic 也能
# 让 Dock、应用列表、Alt-Tab 用上新图标；正式安装用 deb 会把同一套装到 /usr/share。
set -euo pipefail
ROOT="$(cd "$(dirname "$0")" && pwd)"
DATA="${XDG_DATA_HOME:-$HOME/.local/share}"
APP_ID=io.github.owl.owlmusic

install -d "$DATA/icons/hicolor/scalable/apps" "$DATA/applications"
install -m 0644 "$ROOT/resources/$APP_ID.svg" \
           "$DATA/icons/hicolor/scalable/apps/$APP_ID.svg"

for s in 16 24 32 48 64 128 256 512; do
  src="$ROOT/resources/icons/hicolor/${s}x${s}/apps/$APP_ID.png"
  [ -f "$src" ] || continue
  install -d "$DATA/icons/hicolor/${s}x${s}/apps"
  install -m 0644 "$src" "$DATA/icons/hicolor/${s}x${s}/apps/"
done

install -m 0644 "$ROOT/resources/$APP_ID.desktop" \
           "$DATA/applications/$APP_ID.desktop"

if command -v gtk-update-icon-cache >/dev/null 2>&1; then
  gtk-update-icon-cache -q -t -f "$DATA/icons/hicolor" || true
fi
if command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database -q "$DATA/applications" || true
fi

echo "图标已安装到 $DATA/icons/hicolor/$APP_ID"
echo "若 Dock 还显示旧图，重启一下应用（必要时注销重登刷新 GNOME Shell 图标缓存）。"
