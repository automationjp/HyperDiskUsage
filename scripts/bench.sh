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
#   - Runs cases: turbo without uring, turbo with uring, and optional rayon-par build.
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
  local total=0
  for i in $(seq 1 "$RUNS"); do
    local t0=$(date +%s%N)
    "${cmd[@]}" >/dev/null 2>&1 || true
    local t1=$(date +%s%N)
    local dt=$(( (t1 - t0)/1000000 ))
    echo "  run $i: ${dt} ms"
    total=$(( total + dt ))
  done
  echo "  avg: $(( total / RUNS )) ms"
}

if [[ -z "$BIN" ]]; then
  BIN=$(command -v hyperdu-cli || true)
fi
if [[ -z "$BIN" ]]; then
  echo "error: hyperdu-cli not found in PATH; build first"
  exit 1
fi

bench_one "turbo-off" "$BIN" "$ROOT" --perf turbo --no-uring
HYPERDU_USE_URING=1 bench_one "turbo-uring" "$BIN" "$ROOT" --perf turbo

if [[ "$WITH_RAYON" == "1" ]]; then
  echo "==> building rayon-par variant"
  cargo build -p hyperdu-cli --release --features rayon-par >/dev/null 2>&1
  BIN2=target/release/hyperdu-cli
  if [[ -x "$BIN2" ]]; then
    HYPERDU_USE_URING=1 bench_one "turbo-uring+rayon-par" "$BIN2" "$ROOT" --perf turbo
  fi
fi

# Classification bench (basic / deep)
bench_one "classify-basic" "$BIN" "$ROOT" --classify basic --class-report /dev/null
bench_one "classify-deep" "$BIN" "$ROOT" --classify deep --class-report /dev/null

# Incremental bench: first snapshot update, then compute delta
DB="/tmp/hyperdu_bench_sled.db"
rm -rf "$DB"
bench_one "incr-update" "$BIN" "$ROOT" --incremental-db "$DB" --update-snapshot
bench_one "incr-delta" "$BIN" "$ROOT" --incremental-db "$DB" --compute-delta
