//! Volume capacity: how much room a filesystem has, as opposed to how much a
//! tree occupies.
//!
//! The scanner answers "what is using space". Deciding whether that matters
//! needs the other half -- a 200 GiB directory is unremarkable on a 4 TiB disk
//! and an emergency on a 250 GiB one. Both callers that need this (the CLI's
//! footer and the MCP server's `list_volumes`) would otherwise carry their own
//! copy of the same platform `cfg` blocks.

use std::path::{Path, PathBuf};

/// A mounted filesystem and its capacity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Volume {
    /// Drive root on Windows (`C:\`), mount point on Unix (`/home`).
    pub mount_point: PathBuf,
    pub total_bytes: u64,
    pub free_bytes: u64,
}

impl Volume {
    /// Bytes in use, saturating because `total` and `free` are sampled
    /// separately and a write between the two can invert them.
    #[must_use]
    pub fn used_bytes(&self) -> u64 {
        self.total_bytes.saturating_sub(self.free_bytes)
    }

    /// Fraction of the volume in use, in `0.0..=1.0`. Zero for a zero-sized
    /// volume rather than NaN, so callers can sort without special-casing.
    #[must_use]
    pub fn used_ratio(&self) -> f64 {
        if self.total_bytes == 0 {
            return 0.0;
        }
        self.used_bytes() as f64 / self.total_bytes as f64
    }
}

/// Total and free bytes for the volume holding `path`.
///
/// Returns `None` when the path is not reachable or the platform is not one of
/// the supported ones, so a caller can degrade rather than report a made-up
/// zero as if it were measured.
#[must_use]
pub fn total_free(path: &Path) -> Option<(u64, u64)> {
    platform::total_free(path)
}

/// Every mounted volume the process can see.
///
/// Order is platform-defined. Volumes that cannot be queried are skipped rather
/// than reported with zeroed figures: an unreadable volume and an empty one are
/// different facts, and only one of them is worth acting on.
#[must_use]
pub fn list() -> Vec<Volume> {
    platform::list()
}

#[cfg(windows)]
mod platform {
    use std::{
        os::windows::ffi::OsStrExt,
        path::{Path, PathBuf},
    };

    use windows::{
        core::PCWSTR,
        Win32::Storage::FileSystem::{GetDiskFreeSpaceExW, GetLogicalDrives},
    };

    use super::Volume;

    pub(super) fn total_free(path: &Path) -> Option<(u64, u64)> {
        let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
        // The API wants a directory, and a bare `C:` means "current directory on
        // C:" rather than its root -- a different volume when a drive is
        // substituted.
        if let Some(&ch) = wide.last() {
            if ch != u16::from(b'\\') && ch != u16::from(b'/') {
                wide.push(u16::from(b'\\'));
            }
        }
        if *wide.last().unwrap_or(&0) != 0 {
            wide.push(0);
        }

        let mut free_for_caller: u64 = 0;
        let mut total: u64 = 0;
        let mut total_free: u64 = 0;
        // SAFETY: `wide` is NUL-terminated and outlives the call; the three
        // out-parameters are distinct live locals.
        let ok = unsafe {
            GetDiskFreeSpaceExW(
                PCWSTR(wide.as_ptr()),
                Some(&mut free_for_caller),
                Some(&mut total),
                Some(&mut total_free),
            )
            .is_ok()
        };
        // `total_free` rather than `free_for_caller`: the latter is reduced by
        // the user's quota, which describes the account, not the disk.
        ok.then_some((total, total_free))
    }

    pub(super) fn list() -> Vec<Volume> {
        // SAFETY: no arguments, no pointers; returns a bitmask of drive letters.
        let mask = unsafe { GetLogicalDrives() };
        (0..26u32)
            .filter(|bit| mask & (1 << bit) != 0)
            .filter_map(|bit| {
                let letter = char::from(b'A' + u8::try_from(bit).ok()?);
                let root = PathBuf::from(format!("{letter}:\\"));
                // Empty optical and card readers enumerate but cannot be
                // queried; skipping them keeps the list actionable.
                let (total_bytes, free_bytes) = total_free(&root)?;
                Some(Volume {
                    mount_point: root,
                    total_bytes,
                    free_bytes,
                })
            })
            .collect()
    }
}

#[cfg(unix)]
mod platform {
    use std::{
        ffi::CString,
        os::unix::ffi::OsStrExt,
        path::{Path, PathBuf},
    };

    use super::Volume;

    pub(super) fn total_free(path: &Path) -> Option<(u64, u64)> {
        let c_path = CString::new(path.as_os_str().as_bytes()).ok()?;
        // SAFETY: zeroed is a valid starting state for statvfs; `c_path` is
        // NUL-terminated and outlives the call.
        let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
        let rc = unsafe { libc::statvfs(c_path.as_ptr(), &mut stat as *mut _) };
        if rc != 0 {
            return None;
        }

        // f_frsize is the fragment size the block counts are expressed in;
        // f_bsize is the preferred I/O size and gives wrong totals here.
        let frag = u128::from(stat.f_frsize);
        let total = u128::from(stat.f_blocks) * frag;
        // f_bfree, not f_bavail: the reserved-for-root blocks are still real
        // capacity, matching what the Windows branch reports.
        let free = u128::from(stat.f_bfree) * frag;
        Some((
            u64::try_from(total).unwrap_or(u64::MAX),
            u64::try_from(free).unwrap_or(u64::MAX),
        ))
    }

    #[cfg(target_os = "linux")]
    pub(super) fn list() -> Vec<Volume> {
        let Ok(mounts) = std::fs::read_to_string("/proc/mounts") else {
            return fallback_root();
        };

        let mut seen = std::collections::HashSet::new();
        let mut out: Vec<Volume> = mounts
            .lines()
            .filter_map(parse_mount_line)
            .filter(|mount| seen.insert(mount.clone()))
            .filter_map(|mount| {
                let (total_bytes, free_bytes) = total_free(&mount)?;
                // Pseudo filesystems that slipped the device check report zero
                // capacity; they are noise in a "what is filling up" answer.
                (total_bytes > 0).then_some(Volume {
                    mount_point: mount,
                    total_bytes,
                    free_bytes,
                })
            })
            .collect();

        if out.is_empty() {
            out = fallback_root();
        }
        out
    }

    /// Pull the mount point out of a `/proc/mounts` line, keeping only entries
    /// backed by a real device. `tmpfs`, `proc`, `cgroup` and friends have no
    /// disk behind them, and listing them buries the volumes that do.
    #[cfg(target_os = "linux")]
    pub(super) fn parse_mount_line(line: &str) -> Option<PathBuf> {
        let mut fields = line.split_whitespace();
        let device = fields.next()?;
        let mount_point = fields.next()?;
        if !device.starts_with('/') {
            return None;
        }
        // /proc/mounts octal-escapes the characters that would otherwise break
        // its own field separation.
        let decoded = mount_point
            .replace("\\040", " ")
            .replace("\\011", "\t")
            .replace("\\012", "\n")
            .replace("\\134", "\\");
        Some(PathBuf::from(decoded))
    }

    /// Other Unixes need `getmntinfo`/`getmntent` variants this crate does not
    /// bind yet. Reporting the root volume is honest and still useful; claiming
    /// there are no volumes would not be.
    #[cfg(not(target_os = "linux"))]
    pub(super) fn list() -> Vec<Volume> {
        fallback_root()
    }

    fn fallback_root() -> Vec<Volume> {
        let root = PathBuf::from("/");
        total_free(&root)
            .map(|(total_bytes, free_bytes)| Volume {
                mount_point: root,
                total_bytes,
                free_bytes,
            })
            .into_iter()
            .collect()
    }
}

#[cfg(not(any(windows, unix)))]
mod platform {
    use std::path::Path;

    use super::Volume;

    pub(super) fn total_free(_path: &Path) -> Option<(u64, u64)> {
        None
    }

    pub(super) fn list() -> Vec<Volume> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn used_bytes_is_total_minus_free() {
        let v = Volume {
            mount_point: PathBuf::from("/"),
            total_bytes: 1000,
            free_bytes: 250,
        };
        assert_eq!(v.used_bytes(), 750);
    }

    #[test]
    fn used_bytes_saturates_when_free_exceeds_total() {
        // total and free are sampled separately, so this ordering is reachable
        // on a live filesystem and must not underflow.
        let v = Volume {
            mount_point: PathBuf::from("/"),
            total_bytes: 100,
            free_bytes: 400,
        };
        assert_eq!(v.used_bytes(), 0);
        assert_eq!(v.used_ratio(), 0.0);
    }

    #[test]
    fn used_ratio_is_zero_for_zero_sized_volume() {
        let v = Volume {
            mount_point: PathBuf::from("/"),
            total_bytes: 0,
            free_bytes: 0,
        };
        assert_eq!(v.used_ratio(), 0.0);
    }

    #[test]
    fn used_ratio_reports_half_full() {
        let v = Volume {
            mount_point: PathBuf::from("/"),
            total_bytes: 200,
            free_bytes: 100,
        };
        assert!((v.used_ratio() - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn total_free_reports_capacity_for_an_existing_directory() {
        let dir = tempfile::tempdir().expect("temp dir");
        let Some((total, free)) = total_free(dir.path()) else {
            // Unsupported platform: the contract is None, not a fabricated zero.
            return;
        };
        assert!(total > 0, "a mounted filesystem has non-zero capacity");
        assert!(free <= total, "free space cannot exceed capacity");
    }

    #[test]
    fn total_free_returns_none_for_a_missing_path() {
        let missing = Path::new("/hyperdu-nonexistent-volume-probe-9f3a2b");
        assert!(total_free(missing).is_none());
    }

    #[test]
    fn listed_volumes_are_internally_consistent() {
        let volumes = list();
        if volumes.is_empty() {
            // Platform without enumeration support; nothing to assert.
            return;
        }
        assert!(
            volumes.iter().all(|v| v.total_bytes > 0),
            "a listed volume must have been queried successfully"
        );
        assert!(
            volumes.iter().all(|v| v.free_bytes <= v.total_bytes),
            "free space cannot exceed capacity"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn parse_mount_line_keeps_device_backed_mounts() {
        let line = "/dev/sda1 /boot ext4 rw,relatime 0 0";
        assert_eq!(
            super::platform::parse_mount_line(line),
            Some(PathBuf::from("/boot"))
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn parse_mount_line_skips_pseudo_filesystems() {
        // These have no disk behind them; counting them as volumes would bury
        // the ones that can actually fill up.
        for line in [
            "proc /proc proc rw,nosuid 0 0",
            "tmpfs /run tmpfs rw,nosuid 0 0",
            "cgroup2 /sys/fs/cgroup cgroup2 rw 0 0",
        ] {
            assert_eq!(super::platform::parse_mount_line(line), None, "{line}");
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn parse_mount_line_decodes_escaped_spaces() {
        let line = "/dev/sdb1 /mnt/my\\040drive ext4 rw 0 0";
        assert_eq!(
            super::platform::parse_mount_line(line),
            Some(PathBuf::from("/mnt/my drive"))
        );
    }
}
