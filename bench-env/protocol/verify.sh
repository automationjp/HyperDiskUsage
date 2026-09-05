#!/usr/bin/env bash
# Check that this machine can produce valid measurements. Run it before a
# measurement session, not after wondering why the numbers moved.
#
# Usage:
#   ./verify.sh [--warm-check PATH]
#
# Options:
#   --warm-check PATH  Additionally time three warm `find` passes over PATH and
#                      check they agree within 3%. The protocol reports warm
#                      runs as a minimum, which is only honest while warm runs
#                      are tight.
#   -h, --help         Show this help.
#
# Exits non-zero if anything would invalidate a measurement. A warning here
# would be read past, so these are failures.

set -uo pipefail

usage() { awk 'NR<=17 && /^#( |$)/ { sub(/^# ?/, ""); print }' "$0"; }

WARM_CHECK=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --warm-check) WARM_CHECK="$2"; shift 2;;
    -h|--help)    usage; exit 0;;
    *) echo "error: unknown argument '$1'" >&2; usage >&2; exit 2;;
  esac
done

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=./lib.sh
source "$SCRIPT_DIR/lib.sh"

fail=0
note() { printf '  %-28s %s\n' "$1" "$2"; }
bad()  { printf '  %-28s %s  <-- FAIL\n' "$1" "$2"; fail=1; }

echo "environment:"
record_env "${WARM_CHECK:-}" | sed 's/^/  /'

echo
echo "checks:"

# Provisioning finished. "Instance is running" is not "instance is ready".
if [[ -f /var/lib/bench-env/ready ]]; then
  note "provisioning" "complete"
elif [[ -d /var/lib/bench-env ]]; then
  bad "provisioning" "still running or failed (see /var/lib/bench-env/setup.log)"
else
  note "provisioning" "not a bench-env instance (skipped)"
fi

if [[ -f /var/lib/bench-env/degraded ]]; then
  bad "instance store" "$(cat /var/lib/bench-env/degraded)"
fi

mounted=""
for fs in xfs ext4 btrfs; do
  mountpoint -q "/mnt/$fs" 2>/dev/null && mounted+="$fs "
done
if [[ -n "$mounted" ]]; then
  note "filesystems" "$mounted"
else
  note "filesystems" "none mounted under /mnt (fine unless you need them)"
fi

# Cold measurement needs this and fails obscurely without it.
if [[ $EUID -eq 0 ]] || sudo -n tee /proc/sys/vm/drop_caches </dev/null >/dev/null 2>&1; then
  note "drop_caches" "permitted"
else
  bad "drop_caches" "not permitted -- cold measurements would silently be warm"
fi

gov=$(cat /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor 2>/dev/null || echo n/a)
case "$gov" in
  performance|n/a) note "cpu governor" "$gov" ;;
  *) bad "cpu governor" "$gov (expected performance)" ;;
esac

thp=$(sed -n 's/.*\[\(.*\)\].*/\1/p' /sys/kernel/mm/transparent_hugepage/enabled 2>/dev/null || echo n/a)
case "$thp" in
  never|n/a) note "transparent hugepages" "$thp" ;;
  *) bad "transparent hugepages" "$thp (expected never)" ;;
esac

if [[ -r /proc/swaps ]] && (( $(grep -vc '^Filename' /proc/swaps) > 0 )); then
  bad "swap" "enabled -- memory pressure would be measured as disk latency"
else
  note "swap" "off"
fi

# Burstable instances are the reason this file exists.
itype=$(_instance_type)
case "$itype" in
  t2.*|t3.*|t3a.*|t4g.*)
    bad "instance type" "$itype is burstable; results drift as credits drain" ;;
  unknown) note "instance type" "unknown (not on EC2?)" ;;
  *) note "instance type" "$itype" ;;
esac

read -r load1 _ < /proc/loadavg
if awk -v l="$load1" 'BEGIN{exit !(l > 0.5)}'; then
  bad "load average" "$load1 -- something else is running"
else
  note "load average" "$load1"
fi

# The minimum is only a fair statistic while warm runs are tight.
if [[ -n "$WARM_CHECK" ]]; then
  if [[ -d "$WARM_CHECK" ]]; then
    find "$WARM_CHECK" -xdev >/dev/null 2>&1   # prime the cache
    samples=""
    for _ in 1 2 3; do
      samples+=" $(_time_once_ms "find '$WARM_CHECK' -xdev")"
    done
    lo=$(echo "$samples" | _min_ms)
    hi=$(echo "$samples" | tr ' ' '\n' | grep -v '^$' | sort -n | tail -1)
    if (( lo > 0 )) && awk -v lo="$lo" -v hi="$hi" 'BEGIN{exit !((hi-lo)/lo > 0.03)}'; then
      bad "warm spread" "${lo}ms..${hi}ms (>3%) -- the minimum is not representative"
    else
      note "warm spread" "${lo}ms..${hi}ms"
    fi
  else
    bad "warm check" "'$WARM_CHECK' is not a directory"
  fi
fi

echo
if [[ $fail -eq 0 ]]; then
  echo "ready to measure."
else
  echo "NOT ready: fix the failures above, or the numbers will not mean what they claim." >&2
fi
exit $fail
