#!/usr/bin/env bash
# Render the app icon (app/lavmfx/assets/lavmfx.svg) to the files the build
# uses: the window icon (128×128 raw RGBA, compiled in) and, given an output
# folder, PNGs for desktop entries / AppImage plus the Windows .ico (also kept
# in assets/ for the exe resource).
# Needs rsvg-convert (librsvg) and ImageMagick (magick).
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
svg="$here/../app/lavmfx/assets/lavmfx.svg"
tmp=$(mktemp -d); trap 'rm -rf "$tmp"' EXIT
rsvg-convert -w 128 -h 128 "$svg" -o "$tmp/128.png"
magick "$tmp/128.png" -depth 8 "rgba:$here/../app/lavmfx/assets/icon-128.rgba"
if [[ $# -ge 1 ]]; then
    out=$1; mkdir -p "$out"
    for s in 16 32 48 64 128 256 512; do rsvg-convert -w $s -h $s "$svg" -o "$out/lavmfx-$s.png"; done
    magick "$out"/lavmfx-{16,32,48,64,128,256}.png "$out/lavmfx.ico"
    cp "$out/lavmfx.ico" "$here/../app/lavmfx/assets/lavmfx.ico"
fi
