#!/usr/bin/env bash
# Fails if the version differs between Cargo.toml, the packaging files, the
# AppStream metadata and (optionally) a release tag given as $1 (e.g. v0.2.0).
set -euo pipefail
cd "$(dirname "$0")/.."
ver=$(tools/version.sh)
ok=1
check() { # name actual
  if [ "$2" != "$ver" ]; then echo "version mismatch: $1 is '$2', Cargo.toml is '$ver'" >&2; ok=0; fi
}
check "PKGBUILD pkgver" "$(sed -n 's/^pkgver=//p' packaging/arch/PKGBUILD)"
check "RPM spec Version" "$(sed -n 's/^Version:[[:space:]]*//p' packaging/fedora/file-manager.spec)"
grep -q "release version=\"$ver\"" data/dev.filemanager.FileManager.metainfo.xml \
  || { echo "metainfo has no <release version=\"$ver\">" >&2; ok=0; }
grep -q "^## \[$ver\]" CHANGELOG.md || { echo "CHANGELOG.md has no section for $ver" >&2; ok=0; }
if [ $# -ge 1 ]; then check "tag" "${1#v}"; fi
[ "$ok" = 1 ] && echo "version $ver is consistent"
exit $((1 - ok))
