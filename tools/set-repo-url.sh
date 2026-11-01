#!/usr/bin/env bash
# Usage: tools/set-repo-url.sh https://github.com/you/luma
# Replaces the placeholder repository URL in the manifest and packaging files.
set -euo pipefail
cd "$(dirname "$0")/.."
url=${1:?usage: set-repo-url.sh https://github.com/OWNER/REPO}
url=${url%/}
placeholder=https://github.com/shahriar-ridom/luma
for f in Cargo.toml data/dev.ridom.Luma.metainfo.xml packaging/arch/PKGBUILD packaging/fedora/luma.spec; do
  sed -i "s|$placeholder|$url|g" "$f"
done
echo "Repository URL set to $url"
