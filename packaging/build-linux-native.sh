#!/usr/bin/env bash
# Portable tarball for THIS machine's kind of system (Arch / CachyOS):
# a release build that uses the system's FFmpeg 9, PipeWire, ALSA, Vulkan.
# Runs on Arch-based distros with ffmpeg 9.x installed. For other distros
# use build-linux-portable.sh (bundles FFmpeg, older glibc).
# Output: dist/lavmfx-<version>-linux-x86_64-arch.tar.gz
set -euo pipefail
source "$(dirname "$0")/common.sh"
name="lavmfx-$version-linux-x86_64-arch"
out="$dist/$name"
(cd "$app" && RUSTFLAGS="${RUSTFLAGS:-} $remap" cargo build --release -p lavmfx)
rm -rf "$out"; mkdir -p "$out"
install -Dm755 "$app/target/release/lavmfx" "$out/lavmfx"
strip "$out/lavmfx"
copy_docs "$out"
install -Dm755 "$root/packaging/install-menu-entry.sh" "$out/install-menu-entry.sh"
tar -C "$dist" -czf "$dist/$name.tar.gz" "$name"
echo "$dist/$name.tar.gz"
