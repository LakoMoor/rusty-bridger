#!/usr/bin/env bash
set -euo pipefail

APP_NAME="rusty-bridger"
VERSION=$(grep '^version' "$(dirname "$0")/../../ui/Cargo.toml" | head -1 | sed 's/.*"\(.*\)"/\1/')
ARCH="amd64"
BINARY="rusty-bridge-ui"
WORKSPACE_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
RELEASE_BIN="$WORKSPACE_ROOT/target/release/$BINARY"
OUT_DIR="$WORKSPACE_ROOT/dist/out"
PKG_DIR="$OUT_DIR/${APP_NAME}_${VERSION}_${ARCH}"
DEB_PATH="$OUT_DIR/${APP_NAME}_${VERSION}_${ARCH}.deb"
ICON_SRC="$WORKSPACE_ROOT/ui/resources/rb128.png"

echo "Building release (v${VERSION})..."
cd "$WORKSPACE_ROOT"
cargo build --release -p rusty-bridge-ui

rm -rf "$PKG_DIR"
mkdir -p "$PKG_DIR/DEBIAN"
mkdir -p "$PKG_DIR/usr/bin"
mkdir -p "$PKG_DIR/usr/share/applications"
mkdir -p "$PKG_DIR/usr/share/pixmaps"

cp "$RELEASE_BIN" "$PKG_DIR/usr/bin/$APP_NAME"
[ -f "$ICON_SRC" ] && cp "$ICON_SRC" "$PKG_DIR/usr/share/pixmaps/$APP_NAME.png"

cat > "$PKG_DIR/DEBIAN/control" <<CTRL
Package: $APP_NAME
Version: $VERSION
Section: utils
Priority: optional
Architecture: $ARCH
Depends: libc6 (>= 2.31), libgtk-3-0, libv4l-0
Maintainer: LakoMoor <lakomoor@gmail.com>
Homepage: https://github.com/LakoMoor/rusty-bridger
Description: Rusty Bridger - VTube Studio face tracking bridge
 Cross-platform bridge between face tracking sources and VTube Studio.
 Supports iPhone (via VTube Studio iOS app) and webcam (ONNX neural tracking).
 Free and open-source alternative to VBridger. Fork of rusty-bridge by ovROG.
CTRL

cat > "$PKG_DIR/usr/share/applications/$APP_NAME.desktop" <<DESKTOP
[Desktop Entry]
Name=Rusty Bridger
GenericName=VTube Studio Bridge
Comment=Face tracking bridge for VTube Studio
Exec=/usr/bin/$APP_NAME
Icon=$APP_NAME
Terminal=false
Type=Application
Categories=Utility;AudioVideo;
Keywords=vtube;vtuber;face tracking;webcam;iphone;
DESKTOP

chmod 755 "$PKG_DIR/usr/bin/$APP_NAME"
dpkg-deb --build --root-owner-group "$PKG_DIR" "$DEB_PATH"
echo "DEB ready: $DEB_PATH"
