#!/usr/bin/env bash
set -euo pipefail

# Enforce stricter import grouping using nightly rustfmt if available.
# Falls back to stable rustfmt if nightly is not present.
#
# Usage:
#   scripts/fmt_strict.sh [--check]

CHECK=0
if [[ ${1:-} == "--check" ]]; then CHECK=1; fi

if command -v rustup >/dev/null 2>&1; then
  if rustup toolchain list | (grep -q '^nightly' || true); then
    if [[ $CHECK -eq 1 ]]; then
      cargo +nightly fmt --all -- --check \
        --config group_imports=StdExternalCrate \
        --config imports_granularity=Crate
    else
      cargo +nightly fmt --all -- \
        --config group_imports=StdExternalCrate \
        --config imports_granularity=Crate
    fi
    exit 0
  fi
fi

echo "(info) nightly rustfmt not found; using stable rustfmt instead" >&2
if [[ $CHECK -eq 1 ]]; then
  cargo fmt --all -- --check
else
  cargo fmt --all
fi
