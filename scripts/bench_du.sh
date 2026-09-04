#!/usr/bin/env bash
set -uo pipefail

# Compare hyperdu against GNU du on the same trees.
#
# Usage:
#   scripts/bench_du.sh [--bin PATH] [--runs N] [--cold] [--allow-stale] TREE [TREE...]
#
# Options:
#   --bin PATH      hyperdu-cli binary (default: target/release/hyperdu-cli)
#   --runs N        Warm iterations per case (default: 5)
#   --cold          Also measure with a cold page cache. Needs root, since it
#                   writes to /proc/sys/vm/drop_caches between runs.
#   --allow-stale   Skip the check that the binary is newer than the sources.
#                   Only for measuring a deliberately older build.
#   -h, --help      Show this help
#
# This script enforces the fairness rules rather than documenting them, because
# every one of them has already been violated at least once:
#
#   - STALE BINARY. `cargo build --features X` overwrites target/release, so a
#     later run measured an older binary and reported the wrong numbers. The
#     binary is now compared against the newest source mtime and the run aborts.
#   - EQUAL WORKLOAD. hyperdu's default exclude list once dropped .github (".git"
#     matched it as a substring), so it scanned 437 fewer directories than du and
#     looked faster for it. Each tree's file count is now checked against find
#     and the run aborts on a mismatch.
#   - COLD IS NOT A MINIMUM. On EBS gp3 the same cold run measured 0.495s twice
#     and then 0.81-0.89s six times as the volume's burst credit ran out. Taking
#     the minimum reports the burst state as if it were steady state. Cold runs
#     therefore burn the burst off first, interleave the two tools, and report
#     the median. Warm runs stay on the minimum: they measured within +/-1.5%
#     across 25 repetitions, so the minimum is representative there.
#   - BOTH TOOLS GET `-x`. A tree hiding a different mount (say
#     /usr/lib/wsl/drivers on WSL2) is otherwise scanned by one tool and skipped
#     by the other.
#
# The totals still differ by design: du counts the blocks of directories
# themselves, hyperdu counts file data. `--compat gnu -b` matches GNU
# byte-for-byte on file data; see README.

usage() { awk 'NR<=41 && /^#( |$)/ { sub(/^# ?/, ""); print }' "$0"; }

BIN="target/release/hyperdu-cli"
RUNS=5
COLD=0
ALLOW_STALE=0
TREES=()

while [[ $# -gt 0 ]]; do
  case "$1" in
    --bin) BIN="$2"; shift 2;;
    --runs) RUNS="$2"; shift 2;;
    --cold) COLD=1; shift;;
    --allow-stale) ALLOW_STALE=1; shift;;
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

repo_root() { git rev-parse --show-toplevel 2>/dev/null || echo "."; }

# Abort when the binary predates the sources. A warning would be read past.
check_not_stale() {
  [[ $ALLOW_STALE -eq 1 ]] && return 0
  local root newest bin_mtime src_mtime
  root=$(repo_root)
  newest=$(find "$root/hyperdu-core/src" "$root/hyperdu-cli/src" \
                "$root/hyperdu-core/Cargo.toml" "$root/hyperdu-cli/Cargo.toml" \
                "$root/Cargo.toml" -type f -newer "$BIN" -print -quit 2>/dev/null)
  [[ -z "$newest" ]] && return 0
  bin_mtime=$(date -r "$BIN" '+%Y-%m-%d %H:%M:%S' 2>/dev/null || echo '?')
  src_mtime=$(date -r "$newest" '+%Y-%m-%d %H:%M:%S' 2>/dev/null || echo '?')
  cat >&2 <<EOF
error: '$BIN' is older than the sources; the numbers would describe a build that
       no longer exists.
         binary : $bin_mtime
         source : $src_mtime  ($newest)
       Rebuild (cargo build --release), or pass --allow-stale if that is the
       point of the run.
EOF
  exit 3
}

# hyperdu excludes nothing by default now, but say so explicitly: a benchmark
# must not depend on whatever the default happens to be at the time.
hyperdu() { "$BIN" "$1" --top 1 --exclude "" --one-file-system "${@:2}"; }

hyperdu_files() {
  hyperdu "$1" 2>/dev/null | sed -n 's/.*files=\([0-9]*\).*/\1/p' | tail -1
}

# du and hyperdu must walk the same set, or the comparison measures the filter.
check_same_workload() {
  local tree=$1 find_files=$2 hd_files
  hd_files=$(hyperdu_files "$tree")
  [[ "$hd_files" == "$find_files" ]] && return 0
  cat >&2 <<EOF
error: hyperdu and find disagree on '$tree'; the two tools are not scanning the
       same set, so any timing from this tree is meaningless.
         find    : $find_files files
         hyperdu : ${hd_files:-<none>} files
       Check the active exclude patterns (hyperdu prints them under "Excludes:").
EOF
  exit 4
}

median_ms() {
  # Reads whitespace-separated integers on stdin.
  tr ' ' '\n' | grep -v '^$' | sort -n | awk '
    { a[NR] = $1 }
    END { print (NR % 2) ? a[(NR + 1) / 2] : int((a[NR / 2] + a[NR / 2 + 1]) / 2) }'
}

time_once_ms() {
  local s
  s=$(date +%s%N)
  "$@" >/dev/null 2>&1
  echo $(( ($(date +%s%N) - s) / 1000000 ))
}

best_ms() {
  local lo=99999999 t
  for _ in $(seq 1 "$RUNS"); do
    t=$(time_once_ms "$@")
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

# Burn off the device's burst allowance, then alternate the tools so both see
# the same steady state, and report medians.
cold_pair_ms() {
  local tree=$1 i du_samples="" hd_samples=""
  for _ in 1 2 3; do
    drop_caches
    du -sx "$tree" >/dev/null 2>&1
  done
  for ((i = 0; i < 5; i++)); do
    drop_caches
    du_samples+=" $(time_once_ms du -sx "$tree")"
    drop_caches
    hd_samples+=" $(time_once_ms hyperdu "$tree")"
  done
  echo "$(echo "$du_samples" | median_ms) $(echo "$hd_samples" | median_ms)"
}

physical_cores() {
  awk -F: '/^core id/ { ids[$2] = 1 } END { n = 0; for (k in ids) n++; print (n ? n : "?") }' \
    /proc/cpuinfo 2>/dev/null || echo '?'
}

# Everything needed to reproduce a run, or to notice that an old one is not
# comparable to a new one.
root=$(repo_root)
echo "host:     $(uname -sr)  vCPU=$(nproc 2>/dev/null || echo '?')  physical=$(physical_cores)"
echo "commit:   $(git -C "$root" rev-parse --short HEAD 2>/dev/null || echo '?')"
dirty=$(git -C "$root" status --porcelain 2>/dev/null | wc -l)
if [[ "$dirty" -gt 0 ]]; then
  echo "tree:     DIRTY ($dirty uncommitted change(s)) -- not reproducible from the commit alone"
else
  echo "tree:     clean"
fi
echo "du:       $(du --version 2>/dev/null | head -1 || echo 'unknown')"
echo "hyperdu:  $BIN  ($(date -r "$BIN" '+%Y-%m-%d %H:%M:%S' 2>/dev/null || echo '?'))"
echo "warm:     minimum of $RUNS runs"
[[ $COLD -eq 1 ]] && echo "cold:     3 burn-in runs, then 5 interleaved pairs, median reported"
echo

check_not_stale

printf '%-24s %-9s %-8s %-8s %8s %8s %7s' 'tree' 'files' 'dirs' 'fs' 'du' 'hyperdu' 'ratio'
[[ $COLD -eq 1 ]] && printf ' %9s %9s %7s' 'du-cold' 'hd-cold' 'ratio'
printf '\n'

for tree in "${TREES[@]}"; do
  files=$(find "$tree" -xdev -type f 2>/dev/null | wc -l)
  dirs=$(find "$tree" -xdev -type d 2>/dev/null | wc -l)
  fstype=$(stat -f -c %T "$tree" 2>/dev/null || echo '?')

  check_same_workload "$tree" "$files"

  du -sx "$tree" >/dev/null 2>&1      # warm both tools identically
  hyperdu "$tree" >/dev/null 2>&1
  du_ms=$(best_ms du -sx "$tree")
  hd_ms=$(best_ms hyperdu "$tree")

  printf '%-24s %-9s %-8s %-8s %7sms %7sms %6.2fx' \
    "$(basename "$tree")" "$files" "$dirs" "$fstype" "$du_ms" "$hd_ms" \
    "$(awk -v a="$du_ms" -v b="$hd_ms" 'BEGIN { print (b>0 ? a/b : 0) }')"

  if [[ $COLD -eq 1 ]]; then
    read -r cdu_ms chd_ms <<<"$(cold_pair_ms "$tree")"
    printf ' %8sms %8sms %6.2fx' "$cdu_ms" "$chd_ms" \
      "$(awk -v a="$cdu_ms" -v b="$chd_ms" 'BEGIN { print (b>0 ? a/b : 0) }')"
  fi
  printf '\n'
done
