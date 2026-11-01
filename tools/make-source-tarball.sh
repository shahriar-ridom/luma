#!/usr/bin/env bash
# Usage: tools/make-source-tarball.sh OUTDIR
# Creates OUTDIR/luma-<version>.tar.gz containing the sources (no build output).
set -euo pipefail
cd "$(dirname "$0")/.."
out=${1:?usage: make-source-tarball.sh OUTDIR}
ver=$(tools/version.sh)
mkdir -p "$out"
out=$(cd "$out" && pwd)
name="luma-$ver"
if git rev-parse --git-dir >/dev/null 2>&1 && [ -n "$(git ls-files | head -n1)" ]; then
  # Tracked plus new, unignored files; --ignore-failed-read tolerates files deleted or renamed in a dirty tree.
  git ls-files -z --cached --others --exclude-standard \
    | tar --null -T - --ignore-failed-read --transform "s,^,$name/," -czf "$out/$name.tar.gz"
else
  tar --exclude=./target --exclude=./.git --exclude=./dist --exclude=./pkg --exclude=./src-pkg \
      --exclude='*.pkg.tar.*' --exclude='*.AppImage' --exclude=./AppDir \
      --transform "s,^\./,$name/," -czf "$out/$name.tar.gz" .
fi
echo "$out/$name.tar.gz"
