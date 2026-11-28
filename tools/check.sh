#!/usr/bin/env bash
# The checks to run before saying a change is done:
#
#   tools/check.sh            # from anywhere in the checkout (Linux, or Git Bash on Windows)
#   tools/check.sh --no-cross # skip the cross-target check (faster)
#
# 1. cargo fmt --check
# 2. clippy (-D warnings) for this system
# 3. cargo test --workspace, listing every test that skipped and why
# 4. on Linux: clippy for Windows (x86_64-pc-windows-gnullvm), when the cross
#    toolchain from packaging/README.md is installed
#
# It ends with a summary. Anything not checked is listed as NOT CHECKED, never
# left out: a skip is not a pass. Systems this machine can't compile for at all
# (macOS everywhere, Linux on Windows) are covered by CI (.github/workflows/ci.yml).
# Exit status: 0 only when every step that ran passed.
set -uo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
app="$root/app"
cross=1
[[ ${1:-} == --no-cross ]] && cross=0
# LAVMFX_CHECK_LOGS: where the logs go (CI uploads them); else a new temp folder.
if [[ -n ${LAVMFX_CHECK_LOGS:-} ]]; then
    logs=$LAVMFX_CHECK_LOGS
    mkdir -p "$logs"
else
    logs=$(mktemp -d "${TMPDIR:-/tmp}/lavmfx-check.XXXXXX")
fi
summary=()
failed=0

run() { # run <name> <log-name> <command…>
    local name=$1 log="$logs/$2.log"
    shift 2
    echo "== $name"
    if (cd "$app" && "$@") >"$log" 2>&1; then
        summary+=("PASS         $name")
    else
        summary+=("FAIL         $name  (log: $log)")
        failed=1
        # Panics (with the lines after them) first: the tail alone loses them.
        grep -E -A8 "panicked at|^---- " "$log" | head -n 200
        echo "…"
        tail -n 30 "$log"
    fi
}
not_checked() { summary+=("NOT CHECKED  $1: $2"); }

case "$(uname -s)" in
    Linux) os=linux ;;
    MINGW* | MSYS* | CYGWIN*) os=windows ;;
    Darwin) os=macos ;;
    *) os=other ;;
esac

run "rustfmt" fmt cargo fmt --all --check
run "clippy ($os)" clippy cargo clippy --workspace --all-targets --locked -- -D warnings

# FFGL test plugins built by tools/ffgl-test-plugins.sh are used when present.
cache=${XDG_CACHE_HOME:-$HOME/.cache}/lavmfx/ffgl-test/plugins
if [[ -z ${LAVMFX_FFGL_TEST:-} && -d $cache ]]; then
    export LAVMFX_FFGL_TEST=$cache
fi
run "tests ($os)" test cargo test --workspace --locked -- --nocapture
# Tests that can't run here print a line with "skipping" and pass: list them.
if [[ -f $logs/test.log ]]; then
    while read -r n line; do
        not_checked "test skipped ${n}×" "$line"
    done < <(grep -io "skipping.*" "$logs/test.log" | sort | uniq -c)
fi

if [[ $os == linux ]]; then
    deps=${LAVMFX_BUILD_DEPS:-$HOME/.local/opt/lavmfx-build}
    if ((cross == 0)); then
        not_checked "Windows" "--no-cross given"
    elif [[ ! -d $deps/llvm-mingw || ! -d $deps/ffmpeg-win ]]; then
        not_checked "Windows" "cross toolchain not installed ($deps; see packaging/README.md)"
    elif ! rustup target list --installed 2>/dev/null | grep -q x86_64-pc-windows-gnullvm; then
        not_checked "Windows" "rustup target add x86_64-pc-windows-gnullvm"
    else
        # Sourced in a subshell: setup.sh changes PATH and the compiler variables.
        run "clippy (Windows, gnullvm)" clippy-windows bash -c '
            source "$0/packaging/common.sh" && source "$0/packaging/wintc/setup.sh" &&
            FFMPEG_DIR="$deps/ffmpeg-win" cargo clippy --workspace --all-targets --locked \
                --target x86_64-pc-windows-gnullvm --target-dir target/windows -- -D warnings' "$root"
    fi
    not_checked "macOS" "can't be compiled here; see CI"
elif [[ $os == windows ]]; then
    not_checked "Linux" "can't be compiled here; see CI"
    not_checked "macOS" "can't be compiled here; see CI"
else
    not_checked "Linux, Windows" "can't be compiled here; see CI"
fi

echo
echo "== Summary (logs in $logs)"
printf '%s\n' "${summary[@]}"
exit $failed
