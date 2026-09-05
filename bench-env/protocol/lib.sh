#!/usr/bin/env bash
# Measurement primitives shared across projects.
#
# Usage:
#   source /path/to/bench-env/protocol/lib.sh
#   record_env /mnt/xfs
#   compare_warm 5 "toolA args" "toolB args"
#   compare_cold   "toolA args" "toolB args"
#
# Every rule below was broken at least once during HyperDiskUsage's own
# measurements, so each is enforced in code rather than written in a comment
# that would be read past.
#
#   - COLD IS NOT A MINIMUM. On a burstable instance the same cold run measured
#     0.495s twice and then 0.81-0.89s six times as the burst allowance ran out.
#     Taking the minimum reports the burst state as if it were steady state.
#     Cold runs therefore burn the burst off first, interleave the tools, and
#     report the median. Warm runs stay on the minimum: they measured within
#     +/-1.5% across 25 repetitions, so the minimum is representative there.
#   - INTERLEAVE. Running all of tool A then all of tool B lets a drifting
#     machine state be attributed to the tool. Alternate them.
#   - RECORD THE ENVIRONMENT. A number without its kernel, core count, instance
#     type and filesystem cannot be compared against a later one.

set -uo pipefail

# --- environment metadata ----------------------------------------------------

_physical_cores() {
  awk -F: '/^core id/ { ids[$2] = 1 } END { n = 0; for (k in ids) n++; print (n ? n : "?") }' \
    /proc/cpuinfo 2>/dev/null || echo '?'
}

# EC2 instance type via IMDSv2. Prints "unknown" off EC2 rather than hanging.
_instance_type() {
  local token
  token=$(curl -s -X PUT "http://169.254.169.254/latest/api/token" \
            -H "X-aws-ec2-metadata-token-ttl-seconds: 60" \
            --max-time 1 2>/dev/null) || { echo unknown; return; }
  [[ -z "$token" ]] && { echo unknown; return; }
  curl -s -H "X-aws-ec2-metadata-token: $token" --max-time 1 \
    "http://169.254.169.254/latest/meta-data/instance-type" 2>/dev/null || echo unknown
}

# Tab-separated key/value lines on stdout. Redirect to a file to keep the
# metadata alongside the results; a number without it cannot be compared later.
record_env() {
  local path=${1:-}
  printf 'timestamp\t%s\n'       "$(date -u '+%Y-%m-%dT%H:%M:%SZ')"
  printf 'kernel\t%s\n'          "$(uname -sr)"
  printf 'vcpu\t%s\n'            "$(nproc 2>/dev/null || echo '?')"
  printf 'physical_cores\t%s\n'  "$(_physical_cores)"
  printf 'instance_type\t%s\n'   "$(_instance_type)"
  printf 'cpu_governor\t%s\n' \
    "$(cat /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor 2>/dev/null || echo 'n/a')"
  printf 'thp\t%s\n' \
    "$(sed -n 's/.*\[\(.*\)\].*/\1/p' /sys/kernel/mm/transparent_hugepage/enabled 2>/dev/null || echo 'n/a')"
  [[ -n "$path" ]] && printf 'fstype\t%s\n' "$(stat -f -c %T "$path" 2>/dev/null || echo '?')"
  return 0
}

# --- timing primitives -------------------------------------------------------

_time_once_ms() {
  local s
  s=$(date +%s%N)
  eval "$@" >/dev/null 2>&1
  echo $(( ($(date +%s%N) - s) / 1000000 ))
}

_median_ms() {
  tr ' ' '\n' | grep -v '^$' | sort -n | awk '
    { a[NR] = $1 }
    END { print (NR % 2) ? a[(NR + 1) / 2] : int((a[NR / 2] + a[NR / 2 + 1]) / 2) }'
}

_min_ms() {
  tr ' ' '\n' | grep -v '^$' | sort -n | head -1
}

_drop_caches() {
  sync
  if [[ $EUID -eq 0 ]]; then
    echo 3 > /proc/sys/vm/drop_caches
  else
    echo 3 | sudo tee /proc/sys/vm/drop_caches >/dev/null
  fi
  sleep 1
}

# --- comparison --------------------------------------------------------------

# compare_warm RUNS "cmd_a" "cmd_b"  ->  "<a_min_ms> <b_min_ms>"
#
# Minimum of RUNS, interleaved. The minimum is the right statistic here only
# because warm runs are tight; verify.sh checks that assumption still holds.
compare_warm() {
  local runs=$1 cmd_a=$2 cmd_b=$3 i a_samples="" b_samples=""
  for ((i = 0; i < runs; i++)); do
    a_samples+=" $(_time_once_ms "$cmd_a")"
    b_samples+=" $(_time_once_ms "$cmd_b")"
  done
  echo "$(echo "$a_samples" | _min_ms) $(echo "$b_samples" | _min_ms)"
}

# compare_cold "cmd_a" "cmd_b"  ->  "<a_median_ms> <b_median_ms>"
#
# Burn the burst allowance off first, then alternate so both commands see the
# same steady state, and report medians. Needs root or passwordless sudo for
# drop_caches.
compare_cold() {
  local cmd_a=$1 cmd_b=$2 i a_samples="" b_samples=""
  for _ in 1 2 3; do
    _drop_caches
    eval "$cmd_a" >/dev/null 2>&1
  done
  for ((i = 0; i < 5; i++)); do
    _drop_caches
    a_samples+=" $(_time_once_ms "$cmd_a")"
    _drop_caches
    b_samples+=" $(_time_once_ms "$cmd_b")"
  done
  echo "$(echo "$a_samples" | _median_ms) $(echo "$b_samples" | _median_ms)"
}
