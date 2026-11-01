#!/usr/bin/env bash
# Usage: tools/set-repo-url.sh https://github.com/you/file-manager
# Replaces the placeholder repository URL in the manifest and packaging files.
set -euo pipefail
cd "$(dirname "$0")/.."
url=${1:?usage: set-repo-url.sh https://github.com/OWNER/REPO}
url=${url%/}
placeholder=https://github.com/OWNER/file-manager
for f in Cargo.toml data/dev.filemanager.FileManager.metainfo.xml packaging/arch/PKGBUILD packaging/fedora/file-manager.spec; do
  sed -i "s|$placeholder|$url|g" "$f"
done
echo "Repository URL set to $url"
