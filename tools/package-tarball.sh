#!/usr/bin/env bash
# Usage: tools/package-tarball.sh OUTDIR [ARCH]
# Packs the already-built release binary into a portable, relocatable tarball
# with an install script. Run `cargo build --release --locked` first.
set -euo pipefail
cd "$(dirname "$0")/.."
out=${1:?usage: package-tarball.sh OUTDIR [ARCH]}
arch=${2:-$(uname -m)}
ver=$(tools/version.sh)
[ -x target/release/file-manager ] || { echo "build the release binary first" >&2; exit 1; }
mkdir -p "$out"
out=$(cd "$out" && pwd)
name="file-manager-$ver-$arch-linux"
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
make install DESTDIR="$stage/$name" PREFIX=/ >/dev/null
install -m755 packaging/linux-binary/install.sh "$stage/$name/install.sh"
(cd "$stage" && tar -czf "$out/$name.tar.gz" "$name")
echo "$out/$name.tar.gz"
