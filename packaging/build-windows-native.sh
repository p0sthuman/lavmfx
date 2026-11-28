#!/usr/bin/env bash
# Build the Windows packages on Windows (run in Git Bash): a portable folder +
# .zip with lavmfx.exe, the FFmpeg DLLs and the Visual C++ runtime DLLs, and a
# per-user setup .exe made from the same folder.
# Needs: rustup (x86_64-pc-windows-msvc), Visual Studio 2022 or its Build Tools
# (C++ tools, Windows SDK, CMake), LLVM (libclang for bindgen, llvm-objdump),
# Python 3, Inno Setup 6, and in $LAVMFX_BUILD_DEPS (~/.local/opt/lavmfx-build):
#   ffmpeg-win/  BtbN FFmpeg win64-gpl-shared (bin/*.dll, lib/*.lib, include/)
# See packaging/README.md for the exact download.
# Output: dist/lavmfx-<version>-windows-x86_64.zip and …-setup.exe
set -euo pipefail
source "$(dirname "$0")/common.sh"
target=x86_64-pc-windows-msvc
deps=${LAVMFX_BUILD_DEPS:-$HOME/.local/opt/lavmfx-build}
ff="$deps/ffmpeg-win"
fail() { echo "$*" >&2; exit 1; }

# Tool locations, asked from Windows instead of assumed.
programs=$(cygpath -u -F 38)    # Program Files
programs86=$(cygpath -u -F 42)  # Program Files (x86)
localapps=$(cygpath -u -F 28)   # %LOCALAPPDATA%
vswhere="$programs86/Microsoft Visual Studio/Installer/vswhere.exe"
[[ -x "$vswhere" ]] || fail "Visual Studio (or its Build Tools) not found"
vs=$(cygpath -u "$("$vswhere" -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 \
    -property installationPath | tr -d '\r')")
llvm=${LLVM_HOME:-$programs/LLVM}
[[ -e "$llvm/bin/libclang.dll" ]] || fail "LLVM not found in $llvm (set LLVM_HOME)"
export LIBCLANG_PATH=$(cygpath -w "$llvm/bin")
command -v cargo >/dev/null || PATH="$HOME/.cargo/bin:$PATH"
command -v cmake >/dev/null || PATH="$PATH:$vs/Common7/IDE/CommonExtensions/Microsoft/CMake/CMake/bin"
# build.rs needs the SDK's resource compiler for the exe icon and version info.
rc=$(ls "$programs86/Windows Kits/10/bin"/10.*/x64/rc.exe | sort -V | tail -1)
export RC=$(cygpath -w "$rc")
iscc=${ISCC:-$(command -v iscc || true)}
for d in "$localapps/Programs" "$programs86" "$programs"; do
    [[ -n "$iscc" ]] || { [[ -x "$d/Inno Setup 6/ISCC.exe" ]] && iscc="$d/Inno Setup 6/ISCC.exe"; } || true
done
[[ -n "$iscc" ]] || fail "Inno Setup 6 (ISCC.exe) not found (set ISCC)"
# `python3` may be the Microsoft Store stub, which only prints a hint.
export PYTHON=python3
"$PYTHON" -c '' >/dev/null 2>&1 || PYTHON=python
"$PYTHON" -c '' >/dev/null 2>&1 || fail "Python 3 not found"

[[ -d "$ff/bin" ]] || fail "FFmpeg not found in $ff"
export FFMPEG_DIR=$(cygpath -w "$ff")
export LAVMFX_GIT_COMMIT="$git_commit"

# rustc sees Windows paths, so remap those: the home folder (as in common.sh)
# and the checkout, which may be on another drive. The linker would also write
# the .pdb's full path into the exe; keep only its name. Flags are separated by
# \x1f so that paths with spaces survive.
sep=$'\x1f'
export CARGO_ENCODED_RUSTFLAGS="--remap-path-prefix=$(cygpath -w "$HOME")=~"
CARGO_ENCODED_RUSTFLAGS+="${sep}--remap-path-prefix=$(cygpath -w "$root")=lavmfx"
CARGO_ENCODED_RUSTFLAGS+="${sep}-Clink-arg=/PDBALTPATH:%_PDB%"
# Same for the C++ code (Ableton Link's asserts keep __FILE__): cl.exe reads
# `_CL_`, and /d1trimfile cuts the prefix from __FILE__. Cargo doesn't track
# this variable, so remove target/windows-msvc after changing it.
export _CL_="\"/d1trimfile:$(cygpath -w "$HOME")\""
export MSYS2_ENV_CONV_EXCL='CARGO_ENCODED_RUSTFLAGS;_CL_'
(cd "$app" && cargo build --release --locked -p lavmfx --target $target --target-dir target/windows-msvc)

name="lavmfx-$version-windows-x86_64"
out="$dist/$name"
rm -rf "$out"; mkdir -p "$out"
cp "$app/target/windows-msvc/$target/release/lavmfx.exe" "$out/"
cp "$ff"/bin/*.dll "$out/"
crt=$(ls -d "$vs"/VC/Redist/MSVC/14.*/x64/Microsoft.VC14*.CRT | sort -V | tail -1)
for d in msvcp140.dll vcruntime140.dll vcruntime140_1.dll; do cp "$crt/$d" "$out/"; done
install -Dm644 "$root/README.md" "$out/README.md"
copy_licenses "$out" $target
# Double-click tests for output-window problems (DX12 / present mode), each saving its log.
mkdir -p "$out/troubleshooting" && cp "$root"/packaging/windows-tests/*.bat "$out/troubleshooting/"
install -Dm644 "$ff/LICENSE.txt" "$out/licenses/FFmpeg-LICENSE.txt"

# Every DLL the exe needs must be in the folder or part of Windows.
imports=$("$llvm/bin/llvm-objdump.exe" -p "$out"/*.exe "$out"/*.dll | tr -d '\r' |
    sed -n 's/^[[:space:]]*DLL Name: //p' | sort -fu)
[[ -n "$imports" ]] || fail "could not read the DLL imports"
system32=$(cygpath -u -S)
missing=$(while read -r d; do
    [[ -e "$out/$d" || "${d,,}" == api-ms-win-* ]] && continue
    # The C++ runtime is in System32 only where it was installed: not part of Windows.
    [[ -e "$system32/$d" && ! "${d,,}" =~ ^(msvcp|vcruntime|concrt|vcomp) ]] || echo "$d"
done <<<"$imports")
[[ -z "$missing" ]] || fail "DLLs not in the package: $missing"
# No build-machine paths in the program.
# (Not `grep -iF`: Git Bash's grep can abort on it, which would pass the check.)
"$PYTHON" - "$(cygpath -w "$out/lavmfx.exe")" "$(cygpath -w "$HOME")" "$(cygpath -w "$root")" <<'EOF' ||
import sys
data = open(sys.argv[1], "rb").read().lower().replace(b"\\", b"/")
found = [p for p in sys.argv[2:]
         for enc in ("utf-8", "utf-16-le")
         if p.replace("\\", "/").lower().encode(enc) in data]
sys.exit(f"lavmfx.exe contains build paths: {sorted(set(found))}" if found else 0)
EOF
    fail "build-path check failed"

rm -f "$dist/$name.zip" "$dist/$name-setup.exe"
(cd "$dist" && "$system32/tar.exe" -a -cf "$name.zip" "$name")
MSYS2_ARG_CONV_EXCL='*' "$iscc" /Qp "/DAppVersion=$version" "/DSourceDir=$(cygpath -w "$out")" \
    "/DOutputDir=$(cygpath -w "$dist")" "/DOutputName=$name-setup" "$(cygpath -w "$root/packaging/windows-installer.iss")"
echo "$dist/$name.zip"
echo "$dist/$name-setup.exe"
