#!/bin/bash
# Renders the app icon and packs it into assets/ClipVault.icns.
# Only needs rerunning when examples/icongen.rs changes.
set -euo pipefail

cd "$(dirname "$0")/.."
mkdir -p assets

MASTER="assets/icon-1024.png"
ICONSET="$(mktemp -d)/ClipVault.iconset"
mkdir -p "$ICONSET"

echo "==> rendering the master"
cargo run --quiet --example icongen -- "$MASTER"

echo "==> resampling"
# The names are fixed by iconutil: it reads the iconset by filename.
for size in 16 32 128 256 512; do
    sips -z $size $size          "$MASTER" --out "$ICONSET/icon_${size}x${size}.png"      >/dev/null
    sips -z $((size*2)) $((size*2)) "$MASTER" --out "$ICONSET/icon_${size}x${size}@2x.png" >/dev/null
done

echo "==> packing"
iconutil --convert icns "$ICONSET" --output assets/ClipVault.icns
rm -rf "$(dirname "$ICONSET")"

echo "built assets/ClipVault.icns ($(du -h assets/ClipVault.icns | cut -f1))"
