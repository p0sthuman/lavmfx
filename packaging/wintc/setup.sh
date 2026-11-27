# Sourced by build-windows.sh: cross-compile for Windows with the SYSTEM
# clang / lld / llvm-ar, using llvm-mingw only as a sysroot (Windows headers,
# import libraries, libc++ / libunwind / compiler-rt). No mingw-gcc or sudo.
deps=${LAVMFX_BUILD_DEPS:-$HOME/.local/opt/lavmfx-build}
export LM_SYSROOT="$deps/llvm-mingw/x86_64-w64-mingw32"
lm_rt=$(ls -d "$deps"/llvm-mingw/lib/clang/*/lib/windows | head -1)
# Resource dir = the system clang's own headers + llvm-mingw's Windows compiler-rt.
export LM_RESOURCE="$deps/clang-resource-win"
if [[ ! -d "$LM_RESOURCE/lib/windows" ]]; then
    rm -rf "$LM_RESOURCE"; mkdir -p "$LM_RESOURCE/lib"
    cp -r "$(clang -print-resource-dir)/include" "$LM_RESOURCE/"
    cp -r "$lm_rt" "$LM_RESOURCE/lib/windows"
    # bindgen can't lay out AMX's 64-byte-aligned packed tile struct (ffmpeg-sys):
    # hide the AMX headers while generating bindings only.
    sed -i -E 's|^#include <(amx[a-z0-9_-]*intrin\.h)>|#ifndef __LAVMFX_BINDGEN__\n#include <\1>\n#endif|' "$LM_RESOURCE/include/immintrin.h"
fi
tc="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
export PATH="$tc:$PATH"
t=x86_64_pc_windows_gnullvm
export CC_$t="$tc/x86_64-w64-mingw32-clang" CXX_$t="$tc/x86_64-w64-mingw32-clang++" AR_$t=llvm-ar RC_$t=llvm-rc
export CARGO_TARGET_X86_64_PC_WINDOWS_GNULLVM_LINKER="$tc/x86_64-w64-mingw32-clang"
export BINDGEN_EXTRA_CLANG_ARGS_$t="--target=x86_64-w64-mingw32 --sysroot=$LM_SYSROOT -resource-dir=$LM_RESOURCE -isystem $LM_SYSROOT/include -D__LAVMFX_BINDGEN__"
export CARGO_TARGET_X86_64_PC_WINDOWS_GNULLVM_RUSTFLAGS="-C link-arg=-lc++ ${remap:-}"  # rusty_link (Ableton Link, C++) asks for no C++ runtime on this target
