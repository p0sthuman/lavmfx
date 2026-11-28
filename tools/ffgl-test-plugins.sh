#!/usr/bin/env bash
# Builds the FFGL SDK's sample plugins as Linux .so files, for the FFGL host
# test and for trying FFGL in the app:
#
#   tools/ffgl-test-plugins.sh [out-dir]        # default ~/.cache/lavmfx/ffgl-test
#   LAVMFX_FFGL_TEST=<out-dir>/plugins cargo test -p lavmfx-io loads_and_runs -- --nocapture
#
# The SDK (github.com/resolume/ffgl, BSD) is cloned at a pinned commit into
# out-dir at run time; nothing from it is kept in this repository.
# Needs git, g++, GLEW and OpenGL development files (Arch: glew, mesa).
#
# One patch: the SDK only calls glewInit on Windows, so on Linux every GL call
# in a plugin would jump to null. We add a Linux branch that calls glewInit
# too. With our EGL context it loads every GL function and then reports "no
# GLX display", which is fine here. (glewContextInit would skip the GLX step,
# but GLEW 2.2, Ubuntu 24.04's, doesn't declare it.)
set -euo pipefail

commit=5b6e07840277df9ec35a228b9eb067636316350f
out=${1:-${XDG_CACHE_HOME:-$HOME/.cache}/lavmfx/ffgl-test}
src="$out/ffgl"
plugins="$out/plugins"

mkdir -p "$out"
if [[ ! -d $src/.git ]]; then
    git clone -q https://github.com/resolume/ffgl.git "$src"
fi
git -C "$src" fetch -q origin "$commit" 2>/dev/null || true
git -C "$src" checkout -q --force "$commit"

file="$src/source/lib/ffgl/FFGL.cpp"
git -C "$src" checkout -q -- "$file" # an older patch may be applied
if ! grep -q "__linux__" "$file"; then
    # Insert before the "#else" of InitGLExts' "#if defined( FFGL_WINDOWS )".
    awk '
        /^bool InitGLExts\(\)/ { inside = 1 }
        inside && /^#else/ {
            print "#elif defined( __linux__ )"
            print "\tglewExperimental = GL_TRUE;"
            print "\tGLenum r = glewInit();"
            print "\treturn r == GLEW_OK || r == GLEW_ERROR_NO_GLX_DISPLAY;"
            inside = 0
        }
        { print }
    ' "$file" > "$file.new"
    mv "$file.new" "$file"
fi
grep -q "__linux__" "$file" || { echo "patch failed: $file" >&2; exit 1; }

mkdir -p "$plugins"
for p in Add AddSubtract Events Gradients Particles; do
    g++ -std=c++17 -O2 -shared -fPIC -w -I"$src/source/lib" \
        "$src/source/lib/FFGLSDK.cpp" "$src"/source/plugins/"$p"/*.cpp \
        -lGLEW -lGL -o "$plugins/$p.so"
done
ls "$plugins"
