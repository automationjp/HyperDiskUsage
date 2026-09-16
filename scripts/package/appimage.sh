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

# The GUI's own window icon doubles as the AppImage icon, so there is nothing to
# generate. What was here before drew a blank 64x64 square with ImageMagick and,
# when ImageMagick was absent, deleted Icon= from the desktop file "to avoid a
# hard failure inside linuxdeploy". That is backwards: linuxdeploy *requires*
# Icon= and aborts with "Icon entry missing in desktop file" without it. GitHub's
# ubuntu runners have no ImageMagick, so every release took the deleting branch
# and no AppImage has ever been produced.
#
# The filename matters. linuxdeploy compares the icon file's stem against the
# Icon= value, so a 256x256 asset named hyperdu-256.png has to arrive as
# hyperdu.png to match `Icon=hyperdu` above.
icon_src="$root_dir/hyperdu-gui/assets/hyperdu-256.png"
icon_path="$appdir/hyperdu.png"
if [[ ! -f "$icon_src" ]]; then
  echo "error: GUI icon missing at $icon_src" >&2
  exit 1
fi
cp "$icon_src" "$icon_path"

ld_cmd=("$LINUXDEPLOY" --appdir "$appdir" --executable "$appdir/usr/bin/hyperdu-gui" --output appimage)
ld_cmd+=(--desktop-file "$appdir/usr/share/applications/hyperdu-gui.desktop")
ld_cmd+=(--icon-file "$icon_path")

"${ld_cmd[@]}" >> "$dist_dir/linuxdeploy.log" 2>&1 || {
  echo "warn: linuxdeploy failed; AppImage skipped (see dist/linuxdeploy.log)";
  exit 0;
}

mv ./*.AppImage "$dist_dir/" 2>/dev/null || true
echo "AppImage(s) in $dist_dir"
