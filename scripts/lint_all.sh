#!/usr/bin/env bash
set -euo pipefail

# Canonical linter entrypoint (same as push-time checks)
# 1) fmt + clippy for workspace (host target)
# 2) clippy for macOS/Windows targets (core crate only)

repo_root() {
  if rr=$(git rev-parse --show-toplevel 2>/dev/null); then
    echo "$rr"; return
  fi
  local sp="${BASH_SOURCE[0]}"; case "$sp" in /*) ;; *) sp="$PWD/$sp" ;; esac
  cd "$(dirname "$sp")/.." && pwd
}
cd "$(repo_root)"

echo "==> lint (fmt + clippy)"
bash scripts/lint.sh

echo "==> lint (cross: macOS + Windows)"
bash scripts/lint_cross.sh --fast

echo "OK: lint_all passed"

