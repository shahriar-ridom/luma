#!/usr/bin/env bash
# Usage: tools/make-source-tarball.sh OUTDIR
# Creates OUTDIR/file-manager-<version>.tar.gz containing the sources (no build output).
set -euo pipefail
cd "$(dirname "$0")/.."
out=${1:?usage: make-source-tarball.sh OUTDIR}
ver=$(tools/version.sh)
mkdir -p "$out"
out=$(cd "$out" && pwd)
name="file-manager-$ver"
if git rev-parse --git-dir >/dev/null 2>&1 && [ -n "$(git ls-files | head -n1)" ]; then
  git ls-files -z | tar --null -T - --transform "s,^,$name/," -czf "$out/$name.tar.gz"
else
  tar --exclude=./target --exclude=./.git --exclude=./dist --exclude=./pkg --exclude=./src-pkg \
      --exclude='*.pkg.tar.*' --exclude='*.AppImage' --exclude=./AppDir \
      --transform "s,^\./,$name/," -czf "$out/$name.tar.gz" .
fi
echo "$out/$name.tar.gz"
