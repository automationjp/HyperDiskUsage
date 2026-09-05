#!/usr/bin/env bash
# Runs once at boot via cloud-init user-data. Prepares the instance for
# measurement: filesystems on the local NVMe, and kernel settings pinned so
# runs are comparable to each other.
#
# Writes /var/lib/bench-env/ready when finished. launch.sh polls for that file,
# because "the instance is running" and "the instance is ready to measure on"
# are different things, and conflating them means measuring against a
# half-built environment.

set -euo pipefail

STATE_DIR=/var/lib/bench-env
mkdir -p "$STATE_DIR"
exec > >(tee -a "$STATE_DIR/setup.log") 2>&1
echo "=== bench-env setup starting $(date -u '+%Y-%m-%dT%H:%M:%SZ') ==="

# --- packages ----------------------------------------------------------------
# xfsprogs ships with AL2023; btrfs-progs and the profiling tools do not.
dnf install -y --setopt=install_weak_deps=False \
  xfsprogs e2fsprogs btrfs-progs parted git gcc make strace perf jq >/dev/null || {
    echo "warn: some packages failed to install; continuing" >&2
}

# --- filesystems on the local NVMe -------------------------------------------
# The instance store is the point of using an m5d/c5d: mkfs is free here, so
# xfs, ext4 and btrfs can sit side by side for filesystem-specific work.
#
# Device naming is not stable across instance families, so pick the largest
# non-root disk rather than hardcoding /dev/nvme1n1.
root_src=$(findmnt -no SOURCE /)
root_dev=$(lsblk -no PKNAME "$root_src" 2>/dev/null || true)
[[ -n "$root_dev" ]] && root_dev="/dev/$root_dev"

store_dev=$(lsblk -dnpo NAME,TYPE,SIZE --bytes \
            | awk -v root="${root_dev:-none}" '$2=="disk" && $1!=root { print $3, $1 }' \
            | sort -rn | head -1 | awk '{print $2}')

if [[ -z "${store_dev:-}" ]]; then
  echo "warn: no instance-store device found; filesystems not created" >&2
  echo "no-instance-store" > "$STATE_DIR/degraded"
else
  echo "instance store: $store_dev (root is ${root_dev:-unknown})"
  parted -s "$store_dev" mklabel gpt \
    mkpart primary 0%   33% \
    mkpart primary 33%  66% \
    mkpart primary 66% 100%
  udevadm settle
  sleep 2

  mapfile -t parts < <(lsblk -lnpo NAME,TYPE "$store_dev" | awk '$2=="part"{print $1}' | sort)
  if (( ${#parts[@]} < 3 )); then
    echo "warn: expected 3 partitions on $store_dev, found ${#parts[@]}" >&2
    echo "partitioning-failed" > "$STATE_DIR/degraded"
  else
    mkfs.xfs   -f -q "${parts[0]}"
    mkfs.ext4  -F -q "${parts[1]}"
    mkfs.btrfs -f -q "${parts[2]}"

    mkdir -p /mnt/xfs /mnt/ext4 /mnt/btrfs
    mount "${parts[0]}" /mnt/xfs
    mount "${parts[1]}" /mnt/ext4
    mount "${parts[2]}" /mnt/btrfs
    chmod 777 /mnt/xfs /mnt/ext4 /mnt/btrfs
    echo "mounted: /mnt/xfs /mnt/ext4 /mnt/btrfs"
  fi
fi

# --- pin the settings that move measurements ---------------------------------
# Left at their defaults these drift between boots and between instance
# families, and show up as an unexplained few-percent change in results.

# CPU frequency: fix at performance where the driver exposes a governor.
for g in /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor; do
  [[ -w "$g" ]] && echo performance > "$g" 2>/dev/null || true
done

# Transparent huge pages: 'always' adds allocation stalls that land unevenly
# across runs.
[[ -w /sys/kernel/mm/transparent_hugepage/enabled ]] && \
  echo never > /sys/kernel/mm/transparent_hugepage/enabled || true

# Swap would turn a memory-pressure moment into a disk-latency measurement.
swapoff -a 2>/dev/null || true

# Cold measurement needs drop_caches, which is root-only. Grant exactly that.
cat > /etc/sudoers.d/bench-env <<'SUDO'
ec2-user ALL=(root) NOPASSWD: /usr/bin/tee /proc/sys/vm/drop_caches
SUDO
chmod 440 /etc/sudoers.d/bench-env

echo "=== bench-env setup complete $(date -u '+%Y-%m-%dT%H:%M:%SZ') ==="
touch "$STATE_DIR/ready"
