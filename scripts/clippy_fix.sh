#!/usr/bin/env bash
set -euo pipefail

# Auto-apply machine-applicable Clippy fixes using nightly toolchain.
# This will modify files in-place. Intended for use in pre-commit.
#
# Usage:
#   scripts/clippy_fix.sh [--cross]
#     --cross  Also attempt fixes for macOS/Windows targets

CROSS=0
cd_repo_root() {
  # Prefer Git toplevel
  if rr=$(git rev-parse --show-toplevel 2>/dev/null); then
    cd "$rr" || { echo "error: failed to cd to repo root $rr" >&2; exit 1; }
    return
  fi
  # Fallback: derive from this script path
  local sp="${BASH_SOURCE[0]}"
  case "$sp" in /*) ;; *) sp="$PWD/$sp" ;; esac
  local rr
  rr=$(cd "$(dirname "$sp")/.." && pwd) || {
    echo "error: could not resolve repository root" >&2; exit 1; }
  cd "$rr" || { echo "error: cannot cd to $rr" >&2; exit 1; }
}

cd_repo_root
for a in "$@"; do case "$a" in --cross) CROSS=1;; -h|--help) sed -n '1,80p' "$0"; exit 0;; *) echo "unknown arg: $a"; exit 1;; esac; done

command -v rustup >/dev/null 2>&1 || { echo "error: rustup not found" >&2; exit 1; }

# Ensure nightly + clippy present
if ! rustup toolchain list | grep -q '^nightly'; then
  echo "(info) installing nightly toolchain"
  rustup toolchain install nightly -c clippy >/dev/null 2>&1 || true
fi
rustup component add --toolchain nightly clippy >/dev/null 2>&1 || true

echo "==> clippy --fix (workspace, default target)"
cargo +nightly clippy --fix -Z unstable-options --allow-dirty --allow-staged \
  --workspace --all-targets -- -D warnings || true

if [[ $CROSS -eq 1 ]]; then
  # Try cross-target fixes (best-effort). Linking is not required for lint; it may still fail on host.
  for t in x86_64-apple-darwin x86_64-pc-windows-msvc; do
    rustup target add "$t" >/dev/null 2>&1 || true
    echo "==> clippy --fix (target=$t)"
    cargo +nightly clippy --fix -Z unstable-options --allow-dirty --allow-staged \
      --workspace --all-targets --target "$t" -- -D warnings || true
  done
fi

echo "==> rustfmt"
cargo fmt --all || true

echo "OK: clippy fixes applied (if any)"
