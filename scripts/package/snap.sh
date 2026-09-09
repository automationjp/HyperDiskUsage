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

# The snap has to be labelled with the version it was built from. A literal here
# is what stranded the committed snap/snapcraft.yaml at 0.4.0. version-sync:ignore
# This generator rewrites that file on every release run, so editing the
# committed copy could never have stuck.
#
# Ask cargo instead, like brew.sh and scoop.ps1 do -- the crate inherits
# `version.workspace = true`, so the literal is not in hyperdu-cli/Cargo.toml.
# pkgid prints `<url>#<version>`.
version="$(cargo pkgid -p hyperdu-cli --manifest-path "$root_dir/Cargo.toml" | sed 's/.*[#@]//')"
if [[ -z "$version" ]]; then
  echo "error: could not determine hyperdu-cli version from cargo pkgid" >&2
  exit 1
fi

# Quoted heredoc plus a single substitution: only __VERSION__ is interpolated,
# so the rest of the YAML is not at the mercy of shell expansion.
sed "s/__VERSION__/$version/" > "$snap_dir/snapcraft.yaml" <<'YAML'
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
    # The snap ships the CLI only. Without this the plugin falls back to
    # `cargo build --workspace --release`, because its default rust-path of "."
    # makes `cargo read-manifest` fail against a virtual workspace root -- and
    # that drags in hyperdu-gui, whose GTK build dependencies are not declared
    # below. With a real package path the plugin runs `cargo install --locked
    # --path hyperdu-cli --root <install>`, which lands exactly the bin/hyperdu
    # that `apps` and `prime` expect.
    rust-path: [hyperdu-cli]
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
