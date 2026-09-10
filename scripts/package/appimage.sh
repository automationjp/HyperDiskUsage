#!/usr/bin/env bash
set -euo pipefail

# Package hyperdu-gui as an AppImage using linuxdeploy and appimagetool.
# Requires: linuxdeploy, appimagetool (available as AppImages)

usage() {
  cat <<USAGE
Usage: $(basename "$0")

Environment:
  LINUXDEPLOY / APPIMAGETOOL: absolute paths to reviewed local AppImage tools
  LINUXDEPLOY_SHA256 / APPIMAGETOOL_SHA256: independently reviewed SHA256 values
  All four are required, including for preinstalled tools. No downloads or PATH fallback.
  Verification stages private copies and removes them on exit.
USAGE
}

case "${1:-}" in
  -h|--help) usage; exit 0 ;;
  "") ;;
  *) usage >&2; exit 2 ;;
esac

root_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")"/../.. && pwd)"
verified_tools=$(bash "$root_dir/scripts/package/verified-appimage-tools.sh" \
  "${LINUXDEPLOY:-}" "${LINUXDEPLOY_SHA256:-}" \
  "${APPIMAGETOOL:-}" "${APPIMAGETOOL_SHA256:-}")
appdir_root=""
cleanup() {
  rm -rf -- "$verified_tools"
  if [[ -n "$appdir_root" ]]; then rm -rf -- "$appdir_root"; fi
}
trap cleanup EXIT
trap 'exit 1' HUP INT TERM
export LINUXDEPLOY="$verified_tools/linuxdeploy"
export APPIMAGETOOL="$verified_tools/appimagetool"
export PATH="$verified_tools:$PATH"

dist_dir="$root_dir/dist"
mkdir -p "$dist_dir"

cargo build -p hyperdu-gui --release

bin="$root_dir/target/release/hyperdu-gui"
appdir_root="$(mktemp -d)"
appdir="$appdir_root/AppDir"
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

ld_cmd=("$LINUXDEPLOY" --appdir "$appdir" --executable "$appdir/usr/bin/hyperdu-gui" --output appimage)
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
