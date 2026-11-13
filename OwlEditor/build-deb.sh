#!/usr/bin/env bash
# 构建 OwlEditor 的 .deb（本机 / 同版本发行版使用；二进制动态链接系统 GTK4）。
#
# 用法：
#   ./build-deb.sh                          # 先 cargo build --release，再打包
#   OWLEDITOR_BIN=/path/to/owleditor ./build-deb.sh   # 复用已有二进制（跳过编译）
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
VERSION="0.1.0"
ARCH="$(dpkg --print-architecture 2>/dev/null || echo amd64)"
APP_ID="io.github.owl.owleditor"

# 默认先编译：以前脚本只打包现成的 target/release/owleditor，源码改了却打出
# 旧包，装完程序里根本没有新功能（「打开最近文件」就是这么「消失」的）。
if [ -n "${OWLEDITOR_BIN:-}" ]; then
  BIN="$OWLEDITOR_BIN"
else
  BIN="$ROOT/target/release/owleditor"
  export PATH="$HOME/.cargo/bin:$PATH"
  echo "编译 release ..."
  ( cd "$ROOT" && cargo build --release )
fi

if [ ! -x "$BIN" ]; then
  echo "缺少可执行文件: $BIN" >&2
  echo "先运行: cargo build --release   或设置 OWLEDITOR_BIN=<已有二进制>" >&2
  exit 1
fi
echo "打包二进制: $BIN ($(date -r "$BIN" '+%Y-%m-%d %H:%M'))"
command -v dpkg-deb >/dev/null 2>&1 || { echo "需要 dpkg-deb" >&2; exit 1; }

STAGE="$ROOT/target/deb/owleditor_${VERSION}_${ARCH}"
DIST="$ROOT/dist"
rm -rf "$STAGE"
mkdir -p "$STAGE/DEBIAN" "$STAGE/usr/bin" \
         "$STAGE/usr/share/applications" \
         "$STAGE/usr/share/icons/hicolor/scalable/apps" \
         "$DIST"

install -Dm755 "$BIN" "$STAGE/usr/bin/owleditor"
install -Dm644 "$ROOT/resources/$APP_ID.desktop" \
  "$STAGE/usr/share/applications/$APP_ID.desktop"
install -Dm644 "$ROOT/resources/$APP_ID.svg" \
  "$STAGE/usr/share/icons/hicolor/scalable/apps/$APP_ID.svg"
install -Dm644 "$ROOT/resources/$APP_ID-small.svg" \
  "$STAGE/usr/share/icons/hicolor/scalable/apps/$APP_ID-small.svg"

for d in "$ROOT"/resources/icons/hicolor/*/apps; do
  [ -d "$d" ] || continue
  size="$(basename "$(dirname "$d")")"
  mkdir -p "$STAGE/usr/share/icons/hicolor/$size/apps"
  install -m644 "$d/$APP_ID.png" \
    "$STAGE/usr/share/icons/hicolor/$size/apps/$APP_ID.png"
done

cat > "$STAGE/DEBIAN/control" <<EOF
Package: owleditor
Version: $VERSION
Section: utils
Priority: optional
Architecture: $ARCH
Depends: libgtk-4-1, libglib2.0-0, libpango-1.0-0, libcairo2, libgdk-pixbuf-2.0-0, libgraphene-1.0-0
Maintainer: owl <owl@localhost>
Homepage: https://github.com/owl/owleditor
Description: Wayland-native text editor for 10 GiB+ files
 OwlEditor opens multi-gigabyte text files with bounded memory using
 on-demand chunked I/O and an exact full-file line index built
 progressively in the background. Chinese input works natively
 through zwp_text_input_v3 (ibus-rime) without any
 environment-variable workarounds.
EOF

# 安装后立即刷新图标缓存与桌面项数据库。否则 hicolor 主题缓存里仍是上一版
# 图标，dock 会继续显示旧图 / 旧名称（用户反馈「dock 栏图标不正确、名称不对」）。
cat > "$STAGE/DEBIAN/postinst" <<'EOF'
#!/bin/sh
set -e
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
  gtk-update-icon-cache -f -t /usr/share/icons/hicolor >/dev/null 2>&1 || true
fi
if command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database /usr/share/applications >/dev/null 2>&1 || true
fi
EOF
chmod 755 "$STAGE/DEBIAN/postinst"

OUT="$DIST/owleditor_${VERSION}_${ARCH}.deb"
dpkg-deb --build "$STAGE" "$OUT" >/dev/null
echo "已生成: $OUT ($(du -h "$OUT" | cut -f1))"
