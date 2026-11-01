#!/usr/bin/env sh
# Installs Luma from this extracted tarball.
#   ./install.sh                  install for the current user (~/.local)
#   sudo ./install.sh --prefix /usr/local
#   ./install.sh --uninstall [--prefix DIR]
set -eu
here=$(cd "$(dirname "$0")" && pwd)
prefix="${HOME}/.local"
uninstall=0
while [ $# -gt 0 ]; do
  case "$1" in
    --prefix) prefix=${2:?--prefix needs a directory}; shift 2 ;;
    --uninstall) uninstall=1; shift ;;
    -h|--help) sed -n '2,6p' "$0"; exit 0 ;;
    *) echo "unknown option: $1" >&2; exit 1 ;;
  esac
done
appid=dev.ridom.Luma
if [ "$uninstall" = 1 ]; then
  rm -f "$prefix/bin/luma" "$prefix/share/applications/$appid.desktop" "$prefix/share/metainfo/$appid.metainfo.xml"
  (cd "$here/share/icons" && find hicolor -type f \( -name '*.svg' -o -name '*.png' \) -exec rm -f "$prefix/share/icons/{}" \;)
  rm -rf "$prefix/share/doc/luma" "$prefix/share/licenses/luma"
  echo "Luma removed from $prefix"
else
  mkdir -p "$prefix"
  cp -r "$here/bin" "$here/share" "$prefix/"
  if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "$prefix/share/applications" 2>/dev/null || true
  fi
  if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -q -t "$prefix/share/icons/hicolor" 2>/dev/null || true
  fi
  echo "Luma installed to $prefix"
  case ":$PATH:" in *":$prefix/bin:"*) ;; *) echo "Note: add $prefix/bin to your PATH to run 'luma'." ;; esac
fi
