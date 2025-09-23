#!/usr/bin/env bash
set -euo pipefail

# Package hyperdu-gui as an AppImage using linuxdeploy and appimagetool.
# Requires: linuxdeploy, appimagetool (available as AppImages)

usage() {
  cat <<USAGE
Usage: $(basename "$0")

Environment:
  LINUXDEPLOY   Path to linuxdeploy tool (optional)
  APPIMAGETOOL  Path to appimagetool (optional)
USAGE
}

root_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")"/.. && pwd)"
dist_dir="$root_dir/dist"
mkdir -p "$dist_dir"

cargo build -p hyperdu-gui --release

bin="$root_dir/target/release/hyperdu-gui"
appdir="$(mktemp -d)"/AppDir
mkdir -p "$appdir/usr/bin" "$appdir/usr/share/applications"
cp "$bin" "$appdir/usr/bin/hyperdu-gui"

cat > "$appdir/usr/share/applications/hyperdu-gui.desktop" <<DESK
[Desktop Entry]
Type=Application
Name=HyperDU GUI
Exec=hyperdu-gui
Icon=hyperdu
Categories=Utility;System;
DESK

# Try to generate a proper 64x64 PNG icon. Prefer ImageMagick if available.
icon_path=""
if command -v convert >/dev/null 2>&1; then
  convert -size 64x64 canvas:#4a90d9 "$appdir/hyperdu.png" && icon_path="$appdir/hyperdu.png"
elif command -v magick >/dev/null 2>&1; then
  magick -size 64x64 canvas:#4a90d9 "$appdir/hyperdu.png" && icon_path="$appdir/hyperdu.png"
else
  # No ImageMagick; drop Icon= from desktop file to avoid hard failure inside linuxdeploy
  sed -i '/^Icon=/d' "$appdir/usr/share/applications/hyperdu-gui.desktop" || true
fi

linuxdeploy="${LINUXDEPLOY:-linuxdeploy}"
appimagetool="${APPIMAGETOOL:-appimagetool}"

if ! command -v "$linuxdeploy" >/dev/null 2>&1; then
  echo "error: linuxdeploy not found (set LINUXDEPLOY to path)" >&2
  exit 1
fi
if ! command -v "$appimagetool" >/dev/null 2>&1; then
  echo "error: appimagetool not found (set APPIMAGETOOL to path)" >&2
  exit 1
fi

ld_cmd=("$linuxdeploy" --appdir "$appdir" --executable "$appdir/usr/bin/hyperdu-gui" --output appimage)
if [[ -f "$appdir/usr/share/applications/hyperdu-gui.desktop" ]]; then
  ld_cmd+=(--desktop-file "$appdir/usr/share/applications/hyperdu-gui.desktop")
fi
if [[ -n "$icon_path" && -f "$icon_path" ]]; then
  ld_cmd+=(--icon-file "$icon_path")
fi

"${ld_cmd[@]}" >> "$dist_dir/linuxdeploy.log" 2>&1 || {
  echo "warn: linuxdeploy failed; AppImage skipped (see dist/linuxdeploy.log)";
  exit 0;
}

mv ./*.AppImage "$dist_dir/" 2>/dev/null || true
echo "AppImage(s) in $dist_dir"
