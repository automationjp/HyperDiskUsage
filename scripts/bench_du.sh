#!/usr/bin/env bash
set -uo pipefail

# Compare hyperdu against GNU du on the same trees.
#
# Usage:
#   scripts/bench_du.sh [--bin PATH] [--runs N] [--cold] TREE [TREE...]
#
# Options:
#   --bin PATH   hyperdu-cli binary (default: target/release/hyperdu-cli)
#   --runs N     Warm iterations per case, minimum reported (default: 5)
#   --cold       Also measure with a cold page cache. Needs root, since it
#                writes to /proc/sys/vm/drop_caches between runs.
#   -h, --help   Show this help
#
# Fairness notes, all of which matter more than they look:
#   - Both tools get `-x` / `--one-file-system`. A tree that hides a different
#     mount (say /usr/lib/wsl/drivers on WSL2) is otherwise scanned by one tool
#     and skipped by the other, which silently invalidates the comparison.
#   - hyperdu is given `--exclude ""` so its default skip list (.git,
#     node_modules, target) does not shrink its workload.
#   - The minimum of N runs is reported. Single runs on a busy machine vary by
#     2-4x and will tell you whatever you want to hear.
#   - The totals differ by design: du counts the blocks of directories
#     themselves, hyperdu counts file data. `--compat gnu -b` matches GNU
#     byte-for-byte on file data; see README.

usage() { awk 'NR<=32 && /^#( |$)/ { sub(/^# ?/, ""); print }' "$0"; }

BIN="target/release/hyperdu-cli"
RUNS=5
COLD=0
TREES=()

while [[ $# -gt 0 ]]; do
  case "$1" in
    --bin) BIN="$2"; shift 2;;
    --runs) RUNS="$2"; shift 2;;
    --cold) COLD=1; shift;;
    -h|--help) usage; exit 0;;
    *) TREES+=("$1"); shift;;
  esac
done

if [[ ${#TREES[@]} -eq 0 ]]; then
  echo "error: no tree given" >&2
  usage >&2
  exit 2
fi
if [[ ! -x "$BIN" ]]; then
  echo "error: hyperdu binary not found at '$BIN' (build with: cargo build --release)" >&2
  exit 2
fi

hyperdu() { "$BIN" "$1" --top 1 --exclude "" --one-file-system "${@:2}"; }

best_ms() {
  local lo=99999999 t s
  for _ in $(seq 1 "$RUNS"); do
    s=$(date +%s%N)
    "$@" >/dev/null 2>&1
    t=$(( ($(date +%s%N) - s) / 1000000 ))
    (( t < lo )) && lo=$t
  done
  echo "$lo"
}

drop_caches() {
  sync
  if [[ $EUID -eq 0 ]]; then
    echo 3 > /proc/sys/vm/drop_caches
  else
    echo 3 | sudo tee /proc/sys/vm/drop_caches >/dev/null
  fi
  sleep 1
}

cold_ms() {
  local lo=99999999 t s
  for _ in $(seq 1 3); do
    drop_caches
    s=$(date +%s%N)
    "$@" >/dev/null 2>&1
    t=$(( ($(date +%s%N) - s) / 1000000 ))
    (( t < lo )) && lo=$t
  done
  echo "$lo"
}

echo "host:    $(uname -sr)  vCPU=$(nproc 2>/dev/null || echo '?')"
echo "du:      $(du --version 2>/dev/null | head -1 || echo 'unknown')"
echo "hyperdu: $BIN"
echo "runs:    $RUNS (minimum reported)"
echo

printf '%-24s %-9s %-8s %8s %8s %7s' 'tree' 'files' 'dirs' 'du' 'hyperdu' 'ratio'
[[ $COLD -eq 1 ]] && printf ' %9s %9s %7s' 'du-cold' 'hd-cold' 'ratio'
printf '\n'

for tree in "${TREES[@]}"; do
  files=$(find "$tree" -xdev -type f 2>/dev/null | wc -l)
  dirs=$(find "$tree" -xdev -type d 2>/dev/null | wc -l)

  du -sx "$tree" >/dev/null 2>&1      # warm both tools identically
  hyperdu "$tree" >/dev/null 2>&1
  du_ms=$(best_ms du -sx "$tree")
  hd_ms=$(best_ms hyperdu "$tree")

  printf '%-24s %-9s %-8s %7sms %7sms %6.2fx' \
    "$(basename "$tree")" "$files" "$dirs" "$du_ms" "$hd_ms" \
    "$(awk -v a="$du_ms" -v b="$hd_ms" 'BEGIN { print (b>0 ? a/b : 0) }')"

  if [[ $COLD -eq 1 ]]; then
    cdu_ms=$(cold_ms du -sx "$tree")
    chd_ms=$(cold_ms hyperdu "$tree")
    printf ' %8sms %8sms %6.2fx' "$cdu_ms" "$chd_ms" \
      "$(awk -v a="$cdu_ms" -v b="$chd_ms" 'BEGIN { print (b>0 ? a/b : 0) }')"
  fi
  printf '\n'
done
