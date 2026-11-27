#!/usr/bin/env bash
# Optional, for the portable Linux folders: add LAVMFX to your desktop's
# application menu (for this user only), pointing at this folder.
# Undo: rm ~/.local/share/applications/lavmfx.desktop ~/.local/share/icons/hicolor/scalable/apps/lavmfx.svg
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
exe="$here/lavmfx"; [[ -x "$here/AppRun" ]] && exe="$here/AppRun"
apps="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
icons="${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor/scalable/apps"
mkdir -p "$apps" "$icons"
cp "$here/share/icons/hicolor/scalable/apps/lavmfx.svg" "$icons/"
sed "s|^Exec=.*|Exec=\"$exe\" %F|" "$here/share/applications/lavmfx.desktop" > "$apps/lavmfx.desktop"
update-desktop-database "$apps" 2>/dev/null || true
echo "Added LAVMFX to the application menu ($apps/lavmfx.desktop)."
