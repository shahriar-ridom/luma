#!/usr/bin/env bash
# Usage: tools/bump-version.sh 0.2.0
# Updates every place that carries the version, then reminds you to edit the changelog text.
set -euo pipefail
cd "$(dirname "$0")/.."
new=${1:?usage: bump-version.sh X.Y.Z}
[[ $new =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "version must look like 1.2.3" >&2; exit 1; }
today=$(date +%F)
sed -i "0,/^version = \".*\"/s//version = \"$new\"/" Cargo.toml
sed -i "s/^pkgver=.*/pkgver=$new/" packaging/arch/PKGBUILD
sed -i "s/^Version:.*/Version:        $new/" packaging/fedora/luma.spec
sed -i "s|<releases>|<releases>\n    <release version=\"$new\" date=\"$today\"/>|" data/dev.ridom.Luma.metainfo.xml
sed -i "0,/^## \[/s//## [$new] - $today\n\n- TODO: describe this release.\n\n## [/" CHANGELOG.md
cargo update --workspace --offline 2>/dev/null || cargo update --workspace
echo "Bumped to $new. Edit CHANGELOG.md, then run tools/check-version.sh."
