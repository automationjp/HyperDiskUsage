#!/usr/bin/env bash
set -euo pipefail

# Quick micro-benchmark helper for the CLI.
#
# Usage:
#   scripts/bench.sh [--root PATH] [--runs N] [--bin PATH]
#   scripts/bench.sh [PATH]
#
# Options:
#   --root PATH   Root directory to scan (default: '.')
#   --runs N      Iterations per case (default: env RUNS or 3)
#   --bin PATH    Path to hyperdu-cli binary (default: find in PATH)
#   -h, --help    Show this help
#
# Notes:
#   - Runs cases: turbo, and an optional rayon-par build.
#   - Suppresses command output; prints per-run milliseconds and average.

usage() {
  awk 'NR<=40 && /^#( |$)/ { sub(/^# ?/, ""); print }' "$0"
}

ROOT="."
RUNS=${RUNS:-3}
BIN="${BIN:-}"
WITH_RAYON=${WITH_RAYON:-1}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --root) ROOT="$2"; shift 2;;
    --runs) RUNS="$2"; shift 2;;
    --bin)  BIN="$2"; shift 2;;
    -h|--help) usage; exit 0;;
    *) ROOT="$1"; shift;;
  esac
done

bench_one() {
  local name="$1"; shift
  local cmd=("$@")
  echo "==> $name"
  local total=0 best=""
  local out err
  out=$(mktemp) || return 1
  err=$(mktemp) || return 1
  for i in $(seq 1 "$RUNS"); do
    local t0=$(date +%s%N)
    # A failed run finishes early, so counting it as a fast run is worse than
    # useless: it makes a broken build look like an improvement.
    if ! "${cmd[@]}" >"$out" 2>"$err"; then
      local rc=$?
      echo "  run $i: FAILED (exit $rc)" >&2
      sed 's/^/    /' "$err" >&2
      rm -f "$out" "$err"
      return 1
    fi
    local t1=$(date +%s%N)
    local dt=$(( (t1 - t0)/1000000 ))
    echo "  run $i: ${dt} ms"
    total=$(( total + dt ))
    if [[ -z "$best" ]] || (( dt < best )); then best=$dt; fi
  done
  rm -f "$out" "$err"
  echo "  avg: $(( total / RUNS )) ms   min: ${best} ms"
}

if [[ -z "$BIN" ]]; then
  BIN=$(command -v hyperdu-cli || true)
fi
if [[ -z "$BIN" ]]; then
  echo "error: hyperdu-cli not found in PATH; build first"
  exit 1
fi

bench_one "turbo-getdents" "$BIN" "$ROOT" --perf turbo

if [[ "$WITH_RAYON" == "1" ]]; then
  echo "==> building rayon-par variant"
  if cargo build -p hyperdu-cli --release --features rayon-par \
      --target-dir target/bench-rayon >/dev/null; then
    bench_one "turbo+rayon-par" target/bench-rayon/release/hyperdu-cli "$ROOT" --perf turbo
  else
    echo "  (skipped: rayon-par variant failed to build)" >&2
  fi
fi

# Classification bench (basic / deep)
bench_one "classify-basic" "$BIN" "$ROOT" --classify basic --class-report /dev/null
bench_one "classify-deep" "$BIN" "$ROOT" --classify deep --class-report /dev/null
