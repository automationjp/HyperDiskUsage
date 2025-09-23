#!/usr/bin/env bash
set -euo pipefail

# Generate snapcraft.yaml and build snap if snapcraft is installed.

usage() {
  cat <<USAGE
Usage: $(basename "$0") [--generate-only]
USAGE
}

gen_only=0
if [[ ${1:-} == "--generate-only" ]]; then gen_only=1; fi

root_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")"/.. && pwd)"
snap_dir="$root_dir/snap"
mkdir -p "$snap_dir"

cat > "$snap_dir/snapcraft.yaml" <<'YAML'
name: hyperdu
base: core22
version: '0.4.0'
summary: Hyper-fast disk usage analyzer
description: |
  HyperDU is a cross-platform, high-performance disk usage analyzer.

grade: stable
confinement: classic

apps:
  hyperdu:
    command: bin/hyperdu

parts:
  hyperdu:
    plugin: rust
    source: .
    rust-channel: stable
    build-packages: [pkg-config]
    stage-packages: []
    prime:
      - bin/hyperdu
YAML

echo "Wrote $snap_dir/snapcraft.yaml"

if [[ $gen_only -eq 0 ]]; then
  if command -v snapcraft >/dev/null 2>&1; then
    # Prefer 'snapcraft pack' if LXD/MultiPass are not configured; fall back to full build otherwise.
    if groups | grep -q '\blxd\b'; then
      (cd "$root_dir" && snapcraft)
    else
      echo "(info) LXD not configured; attempting 'snapcraft pack'"
      (cd "$root_dir" && snapcraft pack) || echo "warn: snapcraft pack failed; skipping"
    fi
  else
    echo "(info) snapcraft not found; skipping build"
  fi
fi
