#!/usr/bin/env bash
set -euo pipefail

# Build macOS .app for GUI via cargo-bundle and create a DMG if create-dmg is available.

usage() {
  cat <<USAGE
Usage: $(basename "$0") [--release]

Requires: cargo-bundle (cargo install cargo-bundle), create-dmg (brew install create-dmg)
USAGE
}

PROFILE=release
case "${1:-}" in
  --debug) PROFILE=debug ;;
  --release|'') : ;; # default is release
  -h|--help) usage; exit 0 ;;
  *) echo "unknown arg: $1"; usage; exit 1 ;;
esac

cargo install cargo-bundle >/dev/null 2>&1 || true
# Prefer --profile, but fall back if caller environment already injected profile args
set +e
out=$(cargo bundle --profile "$PROFILE" -p hyperdu-gui 2>&1)
rc=$?
set -e
if [[ $rc -ne 0 ]]; then
  echo "$out" >&2
  if echo "$out" | grep -qi 'cannot be used multiple times\|cannot be used with'; then
    echo "(info) retrying cargo bundle with --release"
    cargo bundle --release -p hyperdu-gui
  else
    echo "error: cargo bundle failed" >&2
    exit $rc
  fi
fi

app_path="target/$PROFILE/bundle/osx/hyperdu-gui.app"
outdir="dist"
mkdir -p "$outdir"

if command -v create-dmg >/dev/null 2>&1; then
  dmg="$outdir/hyperdu-gui.dmg"
  rm -f "$dmg"
  create-dmg --overwrite --volname "HyperDU GUI" "$dmg" "$app_path"
  echo "DMG created: $dmg"
else
  zip="$outdir/hyperdu-gui-app.zip"
  (cd "$(dirname "$app_path")" && zip -9 -r "$(pwd)/$zip" "$(basename "$app_path")")
  echo "Zipped app bundle: $zip"
fi
