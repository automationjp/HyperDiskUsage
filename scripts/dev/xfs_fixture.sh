#!/usr/bin/env bash
# Native Linux XFS/async verification on a disposable loop image owned by this run.
# No existing mount or physical block device is formatted or selected.
set -euo pipefail
if [[ $(id -u) != 0 ]]; then
  echo "The owned XFS fixture requires root/CAP_SYS_ADMIN." >&2
  exit 1
fi
# Isolate temporary mount propagation before creating any resource.
if [[ $# == 2 ]]; then
  exec unshare --mount --propagation private -- bash "$0" --private-namespace "$@"
elif [[ $# == 3 && "$1" == --private-namespace ]]; then
  shift
else
  echo "Usage: xfs_fixture.sh CORE_TEST_EXECUTABLE ACCELERATION_TEST_EXECUTABLE" >&2
  exit 1
fi
# Build both test executables as the ordinary user before invoking this harness.
# This avoids running Cargo/build scripts or creating build files as root.
if [[ $# != 2 || ! -x "$1" || ! -x "$2" ]]; then
  echo "Usage: xfs_fixture.sh CORE_TEST_EXECUTABLE ACCELERATION_TEST_EXECUTABLE" >&2
  exit 1
fi
core_test=$1
scan_test=$2
"$core_test" --list | grep -Fx 'platform::linux_x86_64_impl::xfs_bulk::tests::native_owned_xfs_cache_matches_namespace_stat: test' >/dev/null
"$scan_test" --list | grep -Fx 'owned_read_only_xfs_matches_live_metadata_for_every_directory: test' >/dev/null
for tool in mkfs.xfs losetup mount umount findmnt mountpoint truncate python3; do
  command -v "$tool" >/dev/null || { echo "Missing required tool: $tool" >&2; exit 1; }
done
owned=$(mktemp -d /tmp/hyperdu-xfs.XXXXXXXX)
case "$owned" in /tmp/hyperdu-xfs.*) ;; *) echo "Unsafe temporary path" >&2; exit 1;; esac
image="$owned/fixture.img"
root="$owned/mount"
loop=
cleanup() {
  local result=$?
  trap - EXIT
  if mountpoint -q "$root"; then
    if [[ -z "$loop" || $(findmnt -rn -M "$root" -o SOURCE) != "$loop" ]]; then
      echo "Fixture mount identity changed; preserving $owned" >&2
      exit 1
    fi
    if ! umount -- "$root"; then
      echo "Cannot unmount owned fixture; preserving $owned" >&2
      exit 1
    fi
  fi
  if [[ -n "$loop" ]]; then
    if [[ $(losetup -n -O BACK-FILE "$loop") != "$image" ]]; then
      echo "Loop device identity changed; preserving $owned" >&2
      exit 1
    fi
    losetup -d "$loop" || exit 1
  fi
  # Exact owned file and empty directories only; never recursive deletion.
  rm -f -- "$image"
  rmdir -- "$root" "$owned"
  exit "$result"
}
trap cleanup EXIT
mkdir -- "$root"
truncate -s 512M "$image"
[[ -f "$image" && ! -L "$image" ]] || exit 1
mkfs.xfs -q -f "$image"
loop=$(losetup --find --show "$image")
[[ $(losetup -n -O BACK-FILE "$loop") == "$image" ]] || exit 1
mount -t xfs "$loop" "$root"
python3 - "$root" <<'PY'
import os
import pathlib
import socket
import sys
root = pathlib.Path(sys.argv[1])
for index in range(257):
    (root / f"file-{index}").write_bytes(bytes([index % 256]) * (index + 1))
(root / "nested" / "deep").mkdir(parents=True)
(root / "nested" / "deep" / "data").write_bytes(b"x" * 8193)
with (root / "sparse").open("wb") as stream:
    stream.truncate(8 * 1024 * 1024)
    stream.seek(4096)
    stream.write(b"allocated extent")
with (root / "all-hole").open("wb") as stream:
    stream.truncate(1024 * 1024)
os.link(root / "sparse", root / "hardlink")
os.symlink("sparse", root / "symlink")
os.symlink("../..", root / "nested" / "deep" / "cycle")
os.mkfifo(root / "fifo")
with socket.socket(socket.AF_UNIX) as listener:
    listener.bind(str(root / "socket"))
PY
# Unmount/remount establishes a read-only superblock, not a read-only bind mount.
[[ $(findmnt -rn -M "$root" -o SOURCE) == "$loop" ]] || exit 1
umount -- "$root"
mount -t xfs -o ro "$loop" "$root"
findmnt -rn -M "$root" -o SOURCE,FSTYPE,OPTIONS
export HYPERDU_TEST_XFS_ROOT="$root"
# Set HYPERDU_TEST_REQUIRE_URING=1 when native io_uring support is mandatory.
# Otherwise the test explicitly reports an unavailable kernel/security capability.
"$core_test" --exact platform::linux_x86_64_impl::xfs_bulk::tests::native_owned_xfs_cache_matches_namespace_stat --nocapture
"$core_test" --exact platform::linux_x86_64_impl::uring::tests::native_statx_batch_matches_sparse_identity_link_policy_and_errno --nocapture
"$scan_test" --nocapture
