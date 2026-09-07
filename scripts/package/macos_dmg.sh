#!/usr/bin/env bash
set -euo pipefail

# Build macOS .app for GUI via cargo-bundle and create a DMG if create-dmg is available.

# shellcheck disable=SC1090
usage() {
  cat <<USAGE
Usage: $(basename "$0") [--release|--debug]

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

declare -a bundle_args
if [[ $PROFILE == release ]]; then
  bundle_args=(--release)
else
  bundle_args=()
fi

set +e
out=$(cargo bundle "${bundle_args[@]}" -p hyperdu-gui 2>&1)
rc=$?
set -e
if [[ $rc -ne 0 ]]; then
  echo "$out" >&2
  if [[ ${#bundle_args[@]} -gt 0 ]] && \
     echo "$out" | grep -qi "cannot be used .*--profile"; then
    echo "(info) retrying cargo bundle without explicit profile args"
    cargo bundle -p hyperdu-gui
  else
    echo "error: cargo bundle failed" >&2
    exit $rc
  fi
fi

if [[ -d target/release/bundle/osx/hyperdu-gui.app ]]; then
  app_path="target/release/bundle/osx/hyperdu-gui.app"
elif [[ -d target/debug/bundle/osx/hyperdu-gui.app ]]; then
  app_path="target/debug/bundle/osx/hyperdu-gui.app"
else
  echo "error: bundled app not found" >&2
  exit 1
fi
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
