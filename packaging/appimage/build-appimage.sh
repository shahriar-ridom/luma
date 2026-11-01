#!/usr/bin/env bash
# Builds an AppImage from the release binary using linuxdeploy and its GTK plugin.
# Run `cargo build --release --locked` first. Usage: packaging/appimage/build-appimage.sh [OUTDIR]
set -euo pipefail
cd "$(dirname "$0")/../.."
out=${1:-dist}
arch=${ARCH:-$(uname -m)}
ver=$(tools/version.sh)
work=.fm-build/appimage
rm -rf "$work" && mkdir -p "$work" "$out"
out=$(cd "$out" && pwd)

base=https://github.com/linuxdeploy
curl -fsSL -o "$work/linuxdeploy" "$base/linuxdeploy/releases/download/continuous/linuxdeploy-$arch.AppImage"
curl -fsSL -o "$work/linuxdeploy-plugin-gtk.sh" "https://raw.githubusercontent.com/linuxdeploy/linuxdeploy-plugin-gtk/master/linuxdeploy-plugin-gtk.sh"
chmod +x "$work/linuxdeploy" "$work/linuxdeploy-plugin-gtk.sh"

appdir=$work/AppDir
make install DESTDIR="$PWD/$appdir" PREFIX=/usr

extra=()
# Ship bsdtar so archive features work on hosts without libarchive-tools.
if command -v bsdtar >/dev/null 2>&1; then extra+=(--executable "$(command -v bsdtar)"); fi

export DEPLOY_GTK_VERSION=4 VERSION="$ver" APPIMAGE_EXTRACT_AND_RUN=1 ARCH="$arch"
export PATH="$PWD/$work:$PATH"
(cd "$work" && ./linuxdeploy --appdir AppDir \
  --executable AppDir/usr/bin/file-manager \
  --desktop-file AppDir/usr/share/applications/dev.filemanager.FileManager.desktop \
  --icon-file AppDir/usr/share/icons/hicolor/scalable/apps/dev.filemanager.FileManager.svg \
  "${extra[@]}" --plugin gtk --output appimage)

mv "$work"/*.AppImage "$out/file-manager-$ver-$arch.AppImage"
echo "$out/file-manager-$ver-$arch.AppImage"
