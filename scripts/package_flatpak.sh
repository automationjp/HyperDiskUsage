#!/usr/bin/env bash
set -euo pipefail

# Generate a Flatpak manifest and optionally build with flatpak-builder.

usage() {
  cat <<USAGE
Usage: $(basename "$0") [--generate-only]
USAGE
}

gen_only=0
if [[ ${1:-} == "--generate-only" ]]; then gen_only=1; fi

root_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")"/.. && pwd)"
flatpak_dir="$root_dir/packaging/flatpak"
mkdir -p "$flatpak_dir"

cat > "$flatpak_dir/org.hyperdu.GUI.yaml" <<'YAML'
app-id: org.hyperdu.GUI
runtime: org.freedesktop.Platform
runtime-version: '23.08'
sdk: org.freedesktop.Sdk
command: hyperdu-gui
modules:
  - name: hyperdu-gui
    buildsystem: simple
    build-commands:
      - cargo build --release -p hyperdu-gui
      - install -Dm755 target/release/hyperdu-gui /app/bin/hyperdu-gui
    sources:
      - type: dir
        path: ../../
YAML

echo "Wrote $flatpak_dir/org.hyperdu.GUI.yaml"

if [[ $gen_only -eq 0 ]]; then
  if command -v flatpak-builder >/dev/null 2>&1; then
    build_dir=$(mktemp -d)
    # Keep state dir on the same filesystem as build_dir to avoid FS mismatch on WSL/drvfs.
    state_dir="$build_dir/state"
    mkdir -p "$state_dir"
    flatpak-builder --state-dir "$state_dir" "$build_dir" "$flatpak_dir/org.hyperdu.GUI.yaml" --force-clean || true
    echo "(info) Built into $build_dir"
  else
    echo "(info) flatpak-builder not found; skipping build"
  fi
fi
