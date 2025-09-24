#!/usr/bin/env bash
set -euo pipefail

# Cross-target clippy to catch OS-specific issues before CI
# Runs clippy for Windows (msvc) and macOS (darwin) targets from any host.
# Requires: rustup, clippy component, target std installed.

usage() {
  cat <<USAGE
Usage: $(basename "$0") [--fast]

Options:
  --fast   Skip target installation (assumes targets are already installed).

This script lints the workspace for cross targets:
  - x86_64-apple-darwin (macOS)
  - x86_64-pc-windows-msvc (Windows)

Notes:
  - Only metadata is built; linking is not required.
  - If installation of a target fails, that target is skipped with a warning.
USAGE
}

FAST=0
cd_repo_root() {
  if rr=$(git rev-parse --show-toplevel 2>/dev/null); then
    cd "$rr" || { echo "error: failed to cd to repo root $rr" >&2; exit 1; }
    return
  fi
  local sp="${BASH_SOURCE[0]}"
  case "$sp" in /*) ;; *) sp="$PWD/$sp" ;; esac
  local rr
  rr=$(cd "$(dirname "$sp")/.." && pwd) || {
    echo "error: could not resolve repository root" >&2; exit 1; }
  cd "$rr" || { echo "error: cannot cd to $rr" >&2; exit 1; }
}

cd_repo_root
for a in "$@"; do case "$a" in -h|--help) usage; exit 0;; --fast) FAST=1;; *) echo "unknown arg: $a"; usage; exit 1;; esac; done

command -v rustup >/dev/null 2>&1 || { echo "error: rustup not found" >&2; exit 1; }
rustup component add clippy >/dev/null 2>&1 || true

targets=(
  x86_64-apple-darwin
  x86_64-pc-windows-msvc
)

if [[ $FAST -ne 1 ]]; then
  for t in "${targets[@]}"; do
    rustup target add "$t" >/dev/null 2>&1 || echo "(warn) failed to add target $t; will try clippy anyway"
  done
fi

echo "==> clippy (macOS target, core crate only)"
if ! cargo clippy -p hyperdu-core --target x86_64-apple-darwin -- -D warnings; then
  echo "error: clippy failed for x86_64-apple-darwin" >&2
  exit 1
fi

echo "==> clippy (Windows msvc target, core crate only)"
if ! cargo clippy -p hyperdu-core --target x86_64-pc-windows-msvc -- -D warnings; then
  echo "error: clippy failed for x86_64-pc-windows-msvc" >&2
  exit 1
fi

echo "OK: cross-target clippy passed"
