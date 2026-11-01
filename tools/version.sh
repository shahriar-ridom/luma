#!/usr/bin/env bash
# Prints the crate version from Cargo.toml.
set -euo pipefail
cd "$(dirname "$0")/.."
sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1
