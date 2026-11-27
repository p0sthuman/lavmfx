#!/usr/bin/env bash
# Portable Linux build for OTHER distros, made in an Ubuntu 22.04 container
# (podman, rootless) so it needs only glibc 2.35. FFmpeg is bundled in lib/
# (BtbN's generic x86-64 build: CachyOS's own FFmpeg is built for AVX-512 and
# must not be shipped). PipeWire, ALSA, Vulkan come from the user's system.
# Needs: podman; in $LAVMFX_BUILD_DEPS (~/.local/opt/lavmfx-build):
#   ffmpeg-linux/   BtbN linux64-gpl-shared 9.0
#   appimagetool    appimagetool-x86_64.AppImage (optional; run inside the
#                   container with --appimage-extract-and-run)
# Output: dist/lavmfx-<version>-linux-x86_64.tar.gz (+ .AppImage)
set -euo pipefail
source "$(dirname "$0")/common.sh"
deps=${LAVMFX_BUILD_DEPS:-$HOME/.local/opt/lavmfx-build}
image=lavmfx-build:ubuntu22.04
podman build -t "$image" "$root/packaging/linux-portable"

name="lavmfx-$version-linux-x86_64"
mkdir -p "$app/target/portable-ubuntu22" "$HOME/.cargo/registry"
# Compile inside the container. The cargo registry is shared with the host
# (sources only); the target dir is separate from the host build.
podman run --rm \
    -v "$root:/src:Z" \
    -v "$deps/ffmpeg-linux:/deps/ffmpeg:ro,Z" \
    -v "$HOME/.cargo/registry:/opt/cargo/registry:Z" \
    -e FFMPEG_DIR=/deps/ffmpeg \
    -e CARGO_TARGET_DIR=/src/app/target/portable-ubuntu22 \
    -e "LAVMFX_GIT_COMMIT=$git_commit" \
    -e 'RUSTFLAGS=-C link-arg=-Wl,-rpath,$ORIGIN/lib' \
    -w /src/app "$image" \
    cargo build --release -p lavmfx

out="$dist/$name"
rm -rf "$out"; mkdir -p "$out/lib"
install -Dm755 "$app/target/portable-ubuntu22/release/lavmfx" "$out/lavmfx"
strip "$out/lavmfx"
# FFmpeg: copy each library under its soname (what the binary asks for).
for so in "$deps"/ffmpeg-linux/lib/lib*.so.*.*.*; do
    soname=$(objdump -p "$so" | sed -n 's/^ *SONAME *//p')
    cp "$so" "$out/lib/$soname"
done
# The binary's RUNPATH doesn't cover libraries' own dependencies (libavdevice
# → libavfilter …): give each bundled library a RUNPATH of its own folder.
podman run --rm -v "$out/lib:/lib-out:Z" "$image" \
    sh -c 'for f in /lib-out/*.so.*; do patchelf --set-rpath "\$ORIGIN" "$f"; done'
install -Dm644 "$deps/ffmpeg-linux/LICENSE.txt" "$out/licenses/FFmpeg-LICENSE.txt" 2>/dev/null || true
copy_docs "$out"
install -Dm755 "$root/packaging/install-menu-entry.sh" "$out/install-menu-entry.sh"

# Checks: newest glibc symbol needed, and every library resolves inside the
# container (= an Ubuntu 22.04 system) with FFmpeg taken from lib/.
need=$(objdump -T "$out/lavmfx" "$out"/lib/*.so.* | grep -o 'GLIBC_[0-9.]*' | sort -uV | tail -1)
echo "needs $need (Ubuntu 22.04 has GLIBC_2.35)"
podman run --rm -v "$out:/pkg:ro,Z" "$image" sh -c \
    'ldd /pkg/lavmfx | grep -E "not found|libav|libsw"' | tee "$dist/$name.ldd.txt" | grep -q "not found" \
    && { echo "missing libraries on Ubuntu 22.04" >&2; exit 1; }

tar -C "$dist" -czf "$dist/$name.tar.gz" "$name"
cat "$dist/$name.ldd.txt"; rm "$dist/$name.ldd.txt"
echo "$dist/$name.tar.gz"

# AppImage: the same folder as an AppDir, packed by appimagetool in the container.
if [[ -f "$deps/appimagetool" ]]; then
    appdir="$dist/$name.AppDir"
    rm -rf "$appdir"; cp -a "$out" "$appdir"
    cp "$root/packaging/lavmfx.desktop" "$appdir/lavmfx.desktop"
    rsvg-convert -w 256 -h 256 "$app/lavmfx/assets/lavmfx.svg" -o "$appdir/lavmfx.png"
    printf '#!/bin/sh\nHERE=$(dirname "$(readlink -f "$0")")\nexec "$HERE/lavmfx" "$@"\n' > "$appdir/AppRun"
    chmod +x "$appdir/AppRun"
    podman run --rm -v "$dist:/dist:Z" -v "$deps/appimagetool:/tools/appimagetool:ro,Z" \
        -e ARCH=x86_64 -e APPIMAGE_EXTRACT_AND_RUN=1 -w /tmp "$image" \
        /tools/appimagetool --no-appstream "/dist/$name.AppDir" "/dist/$name.AppImage"
    rm -rf "$appdir"
    echo "$dist/$name.AppImage"
else
    echo "(no $deps/appimagetool: skipped the AppImage)"
fi
