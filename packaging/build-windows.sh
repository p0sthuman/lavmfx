#!/usr/bin/env bash
# Cross-build the Windows package on Linux: a portable folder + .zip with
# lavmfx.exe, the FFmpeg DLLs and the C++ runtime DLLs.
# Needs (no sudo): rustup target x86_64-pc-windows-gnullvm, system clang + lld
# + llvm-ar + llvm-rc, cmake, and in $LAVMFX_BUILD_DEPS (~/.local/opt/lavmfx-build):
#   llvm-mingw/  llvm-mingw ucrt release, used only as the Windows sysroot
#   ffmpeg-win/  BtbN FFmpeg win64-gpl-shared (bin/*.dll, lib/*.dll.a, include/)
# See packaging/README.md for the exact downloads.
# Output: dist/lavmfx-<version>-windows-x86_64.zip
set -euo pipefail
source "$(dirname "$0")/common.sh"
source "$root/packaging/wintc/setup.sh"
target=x86_64-pc-windows-gnullvm
ff="$deps/ffmpeg-win"
export FFMPEG_DIR="$ff"
(cd "$app" && cargo build --release -p lavmfx --target $target --target-dir "$app/target/windows")

name="lavmfx-$version-windows-x86_64"
out="$dist/$name"
rm -rf "$out"; mkdir -p "$out"
cp "$app/target/windows/$target/release/lavmfx.exe" "$out/"
llvm-strip "$out/lavmfx.exe"
cp "$ff"/bin/*.dll "$out/"
for d in libc++.dll libunwind.dll; do cp "$LM_SYSROOT/bin/$d" "$out/"; done
install -Dm644 "$root/README.md" "$out/README.md"
copy_licenses "$out" $target
# Double-click tests for output-window problems (DX12 / present mode), each saving its log.
mkdir -p "$out/troubleshooting" && cp "$root"/packaging/windows-tests/*.bat "$out/troubleshooting/"
install -Dm644 "$ff/LICENSE.txt" "$out/licenses/FFmpeg-LICENSE.txt" 2>/dev/null || true
# Every DLL the exe needs must be in the folder or part of Windows.
missing=$(llvm-objdump -p "$out"/*.exe "$out"/*.dll | sed -n 's/^[[:space:]]*DLL Name: //p' | sort -u |
    grep -viE '^(api-ms-win-.*|kernel32|user32|gdi32|advapi32|shell32|ole32|oleaut32|ws2_32|bcrypt|ntdll|winmm|dwmapi|imm32|uxtheme|comctl32|comdlg32|shlwapi|setupapi|propsys|d3d12|d3d11|dxgi|d3dcompiler_47|dxcompiler|opengl32|secur32|crypt32|mfplat|mfreadwrite|mf|mfuuid|strmiids|ksuser|avrt|mmdevapi|iphlpapi|userenv|version|hid|cfgmgr32|winhttp|dbghelp|powrprof|psapi|synchronization|ncrypt|msvcrt|ucrtbase|oleacc|vulkan-1|dnsapi|dwrite|msimg32|rpcrt4|usp10|bcryptprimitives|combase|d2d1|d3d9|dxva2|mfcore|evr|ws2_32|wsock32|normaliz|wldap32|netapi32|mswsock|wtsapi32|shcore|windowscodecs|mfsensorgroup|cryptbase|vfw32|avicap32|msacm32|ksproxy|rtworkq|coremessaging)\.dll$' |
    while read -r d; do [[ -e "$out/$d" ]] || echo "$d"; done)
[[ -z "$missing" ]] || { echo "DLLs not in the package: $missing" >&2; exit 1; }
rm -f "$dist/$name.zip"
(cd "$dist" && bsdtar -a -cf "$name.zip" "$name")
echo "$dist/$name.zip"
