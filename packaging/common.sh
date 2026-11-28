# Shared bits for the build-*.sh scripts (sourced).
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
app="$root/app"
dist="$root/dist"
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$app/Cargo.toml" | head -1)
mkdir -p "$dist"

# Keep build-machine paths (home folder, cargo registry) out of the binaries:
# panic messages and debug info name source files relative to "~" instead.
remap="--remap-path-prefix=$HOME=~"

# Commit for the build stamp, for builds that can't run git themselves.
git_commit=$(git -C "$root" rev-parse --short HEAD 2>/dev/null || echo unknown)
[[ -z $(git -C "$root" status --porcelain --untracked-files=no 2>/dev/null) ]] || git_commit+=-dirty

# Copy the files every package carries into folder $1.
copy_docs() {
    install -Dm644 "$root/README.md" "$1/README.md"
    copy_licenses "$1" x86_64-unknown-linux-gnu
    install -Dm644 "$root/packaging/lavmfx.desktop" "$1/share/applications/lavmfx.desktop"
    install -Dm644 "$app/lavmfx/assets/lavmfx.svg" "$1/share/icons/hicolor/scalable/apps/lavmfx.svg"
}

# Licence files every package carries: LAVMFX's LICENSE, the third-party
# notice, and the licences of the Rust crates built for target $2.
copy_licenses() {
    install -Dm644 "$root/LICENSE" "$1/LICENSE"
    install -Dm644 "$root/packaging/THIRD-PARTY.md" "$1/licenses/THIRD-PARTY.md"
    "${PYTHON:-python3}" "$root/packaging/collect-licenses.py" "$1/licenses/rust-crates.txt" "$2"
}
