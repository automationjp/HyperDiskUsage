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

root_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")"/../.. && pwd)"
snap_dir="$root_dir/snap"
mkdir -p "$snap_dir"

# The crate inherits its version from [workspace.package], so the literal is
# not in hyperdu/Cargo.toml. pkgid prints `<url>#<version>`. This was the
# one packaging script still hardcoding it, and it drifted to 0.4.0 while the
# workspace moved on; every sibling already derives it this way.
VER=$(cd "$root_dir" && cargo pkgid -p hyperdu | sed 's/.*[#@]//')
if [[ -z "$VER" ]]; then echo "error: could not determine hyperdu version" >&2; exit 1; fi

cat > "$snap_dir/snapcraft.yaml" <<'YAML'
name: hyperdu
base: core22
version: '__VERSION__'
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

sed -i.bak -e "s/__VERSION__/$VER/" "$snap_dir/snapcraft.yaml"
rm -f "$snap_dir/snapcraft.yaml.bak"

echo "Wrote $snap_dir/snapcraft.yaml (version $VER)"

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
