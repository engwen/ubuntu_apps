#!/usr/bin/env bash
# 构建 OwlMusic 的 .deb 安装包（不依赖 cargo-deb）
# 用法： ./build-deb.sh [版本号]
set -euo pipefail

APP=owlmusic
VERSION="${1:-$(grep -m1 '^version' Cargo.toml | sed -E 's/.*"(.*)".*/\1/')}"
ARCH=$(dpkg --print-architecture 2>/dev/null || echo amd64)
ROOT="$(cd "$(dirname "$0")" && pwd)"
OUT="$ROOT/dist"
PKG="$OUT/${APP}_${VERSION}_${ARCH}"

# dpkg-deb 打包时要用临时目录，而 /tmp 常常是几 MB 的 tmpfs 容器
# （解包压缩会直接 ENOSPC）。统一放项目内，结束清掉。
export TMPDIR="$ROOT/.tmp-deb"
mkdir -p "$TMPDIR"
trap 'rm -rf "$TMPDIR"' EXIT

echo "==> 编译 release 版本"
cd "$ROOT"
cargo build --release

echo "==> 组装包结构"
rm -rf "$PKG"
install -d "$PKG/DEBIAN" \
           "$PKG/usr/bin" \
           "$PKG/usr/share/applications" \
           "$PKG/usr/share/icons/hicolor/scalable/apps" \
           "$PKG/usr/share/metainfo"

install -m 0755 "target/release/$APP"                    "$PKG/usr/bin/$APP"
install -m 0644 "resources/io.github.owl.owlmusic.desktop" \
          "$PKG/usr/share/applications/io.github.owl.owlmusic.desktop"
install -m 0644 "resources/io.github.owl.owlmusic.svg" \
          "$PKG/usr/share/icons/hicolor/scalable/apps/io.github.owl.owlmusic.svg"
[ -f "resources/io.github.owl.owlmusic.metainfo.xml" ] && \
  install -m 0644 "resources/io.github.owl.owlmusic.metainfo.xml" \
          "$PKG/usr/share/metainfo/io.github.owl.owlmusic.metainfo.xml"

SIZE=$(du -sk "$PKG/usr" | cut -f1)
cat > "$PKG/DEBIAN/control" <<EOF
Package: $APP
Version: $VERSION
Section: sound
Priority: optional
Architecture: $ARCH
Installed-Size: $SIZE
Maintainer: Owl <owl@localhost>
Depends: libgtk-4-1 (>= 4.10), libglib2.0-0, libasound2t64 | libasound2
Recommends: gstreamer1.0-plugins-base
Description: OwlMusic - local music player
 A lightweight local music player for Ubuntu/GNOME, built with GTK4 and Rust.
 Features lyrics with album-art background, tag editing with write-back,
 and online metadata lookup (iTunes / LRCLIB).
EOF

cat > "$PKG/DEBIAN/postinst" <<'EOF'
#!/bin/sh
set -e
if [ -x /usr/bin/update-desktop-database ]; then
  update-desktop-database -q /usr/share/applications || true
fi
if [ -x /usr/bin/gtk-update-icon-cache ]; then
  gtk-update-icon-cache -q -t -f /usr/share/icons/hicolor || true
fi
exit 0
EOF

cat > "$PKG/DEBIAN/postrm" <<'EOF'
#!/bin/sh
set -e
if [ -x /usr/bin/update-desktop-database ]; then
  update-desktop-database -q /usr/share/applications || true
fi
exit 0
EOF

chmod 0755 "$PKG/DEBIAN/postinst" "$PKG/DEBIAN/postrm"

echo "==> 打包"
dpkg-deb --root-owner-group -Zxz --build "$PKG" "$OUT/${APP}_${VERSION}_${ARCH}.deb"
echo "==> 完成：$OUT/${APP}_${VERSION}_${ARCH}.deb"
echo "   安装： sudo dpkg -i $OUT/${APP}_${VERSION}_${ARCH}.deb"
