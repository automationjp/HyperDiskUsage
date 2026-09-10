use std::{ffi::CString, mem::MaybeUninit};

use crate::Options;

// On glibc targets we prefer statx to minimize syscalls and fetch fields efficiently.
// musl targets lack some statx definitions in libc; provide a metadata/fstatat fallback.

/// Build statx mask based on required fields (glibc)
#[cfg(not(target_env = "musl"))]
#[inline]
#[allow(dead_code)]
pub fn build_statx_mask(opt: &Options) -> u32 {
    let mut mask = libc::STATX_SIZE | libc::STATX_MODE;

    if opt.compute_physical {
        mask |= libc::STATX_BLOCKS;
    }

    if !opt.count_hardlinks || opt.one_file_system || opt.visited_dirs.is_some() {
        mask |= libc::STATX_INO;
    }

    mask
}

/// Placeholder on musl; statx is not used on these builds.
#[cfg(target_env = "musl")]
#[inline]
#[allow(dead_code)]
pub fn build_statx_mask(_opt: &Options) -> u32 {
    0
}

/// A checked view whose name cannot outlive the getdents64 buffer.
pub(super) struct DirEntry<'a> {
    pub ino: u64,
    pub offset: u64,
    pub kind: u8,
    pub name: &'a std::ffi::CStr,
    pub record_len: usize,
}

/// Validate the entire record before reading fields or passing its name to FFI.
pub(super) fn decode_dirent(bytes: &[u8]) -> std::io::Result<DirEntry<'_>> {
    let invalid =
        || std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid getdents64 record");
    if bytes.len() < 20 {
        return Err(invalid());
    }
    let record_len = u16::from_ne_bytes([bytes[16], bytes[17]]) as usize;
    if record_len < 20 || record_len > bytes.len() || record_len % 8 != 0 {
        return Err(invalid());
    }
    let field = &bytes[19..record_len];
    let nul = memchr::memchr(0, field).ok_or_else(invalid)?;
    if nul == 0 || field[..nul].contains(&b'/') {
        return Err(invalid());
    }
    let name = std::ffi::CStr::from_bytes_with_nul(&field[..=nul]).map_err(|_| invalid())?;
    Ok(DirEntry {
        ino: u64::from_ne_bytes(bytes[..8].try_into().map_err(|_| invalid())?),
        offset: u64::from_ne_bytes(bytes[8..16].try_into().map_err(|_| invalid())?),
        kind: bytes[18],
        name,
        record_len,
    })
}
/// Perform statx syscall with proper error handling (glibc)
#[cfg(not(target_env = "musl"))]
#[inline]
#[allow(dead_code)]
pub fn do_statx(
    dirfd: libc::c_int,
    pathname: &[u8],
    flags: libc::c_int,
    mask: u32,
) -> Option<libc::statx> {
    let c_name = CString::new(pathname).ok()?;
    let mut stx = MaybeUninit::<libc::statx>::uninit();

    let rc = unsafe {
        libc::statx(
            dirfd,
            c_name.as_ptr(),
            flags | libc::AT_STATX_DONT_SYNC,
            mask,
            stx.as_mut_ptr(),
        )
    };

    if rc == 0 {
        Some(unsafe { stx.assume_init() })
    } else {
        None
    }
}

/// Extract device ID from statx (glibc)
#[cfg(not(target_env = "musl"))]
#[inline]
#[allow(dead_code)]
pub fn statx_dev(stx: &libc::statx) -> u64 {
    ((stx.stx_dev_major as u64) << 32) | (stx.stx_dev_minor as u64)
}

/// Check if current device matches parent for one-file-system.
/// Uses statx on glibc; fstatat metadata fallback on musl.
#[inline]
#[allow(dead_code)]
pub fn check_one_file_system(
    dirfd: libc::c_int,
    name: &[u8],
    parent_dev: u64,
    opt: &Options,
) -> bool {
    if !opt.one_file_system {
        return true; // No check needed
    }

    #[cfg(not(target_env = "musl"))]
    {
        if let Some(stx) = do_statx(dirfd, name, libc::AT_SYMLINK_NOFOLLOW, libc::STATX_INO) {
            return statx_dev(&stx) == parent_dev;
        }
        false
    }

    #[cfg(target_env = "musl")]
    {
        let c_name = match CString::new(name) {
            Ok(s) => s,
            Err(_) => return false,
        };
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        let rc = unsafe {
            libc::fstatat(
                dirfd,
                c_name.as_ptr(),
                &mut st as *mut _,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if rc == 0 {
            let dev = st.st_dev as u64;
            return dev == parent_dev;
        }
        false
    }
}

/// Process directory entry for stats
#[allow(dead_code)]
pub struct EntryStats {
    pub logical: u64,
    pub physical: u64,
    pub dev: u64,
    pub ino: u64,
    pub is_dir: bool,
    pub is_reg: bool,
}

/// Get stats for a directory entry (glibc: statx path)
#[cfg(not(target_env = "musl"))]
#[inline]
#[allow(dead_code)]
pub fn get_entry_stats(
    dirfd: libc::c_int,
    name: &[u8],
    d_type: u8,
    opt: &Options,
) -> Option<EntryStats> {
    let is_dir = d_type == libc::DT_DIR;
    let is_reg = d_type == libc::DT_REG;

    // Fast path for directories with approximate sizes
    if is_dir && !opt.compute_physical && opt.approximate_sizes {
        return Some(EntryStats {
            logical: 0,
            physical: 0,
            dev: 0,
            ino: 0,
            is_dir: true,
            is_reg: false,
        });
    }

    // Fast path for regular files with approximate sizes
    if is_reg && !opt.compute_physical && opt.approximate_sizes && opt.min_file_size == 0 {
        return Some(EntryStats {
            logical: 4096,
            physical: 4096,
            dev: 0,
            ino: 0,
            is_dir: false,
            is_reg: true,
        });
    }

    // Need actual stat
    let mask = build_statx_mask(opt);
    let flags = if opt.follow_links {
        0
    } else {
        libc::AT_SYMLINK_NOFOLLOW
    };
    let stx = do_statx(dirfd, name, flags, mask)?;

    let logical = stx.stx_size;
    let physical = if opt.compute_physical {
        crate::common_ops::calculate_physical_size(opt, logical, stx.stx_blocks)
    } else {
        logical
    };

    Some(EntryStats {
        logical,
        physical,
        dev: statx_dev(&stx),
        ino: stx.stx_ino,
        is_dir: (u32::from(stx.stx_mode) & libc::S_IFMT) == libc::S_IFDIR,
        is_reg: (u32::from(stx.stx_mode) & libc::S_IFMT) == libc::S_IFREG,
    })
}

/// Get stats for a directory entry (musl: fstatat metadata path)
#[cfg(target_env = "musl")]
#[inline]
#[allow(dead_code)]
pub fn get_entry_stats(
    dirfd: libc::c_int,
    name: &[u8],
    d_type: u8,
    opt: &Options,
) -> Option<EntryStats> {
    let is_dir_hint = d_type == libc::DT_DIR;
    let is_reg_hint = d_type == libc::DT_REG;

    // Fast paths mirror the glibc version
    if is_dir_hint && !opt.compute_physical && opt.approximate_sizes {
        return Some(EntryStats {
            logical: 0,
            physical: 0,
            dev: 0,
            ino: 0,
            is_dir: true,
            is_reg: false,
        });
    }
    if is_reg_hint && !opt.compute_physical && opt.approximate_sizes && opt.min_file_size == 0 {
        return Some(EntryStats {
            logical: 4096,
            physical: 4096,
            dev: 0,
            ino: 0,
            is_dir: false,
            is_reg: true,
        });
    }

    let c_name = CString::new(name).ok()?;
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    let flags = if opt.follow_links {
        0
    } else {
        libc::AT_SYMLINK_NOFOLLOW
    };
    let rc = unsafe { libc::fstatat(dirfd, c_name.as_ptr(), &mut st as *mut _, flags) };
    if rc != 0 {
        return None;
    }

    let logical = st.st_size as u64;
    let physical = if opt.compute_physical {
        crate::common_ops::calculate_physical_size(opt, logical, st.st_blocks as u64)
    } else {
        logical
    };

    let mode = st.st_mode as u32;
    Some(EntryStats {
        logical,
        physical,
        dev: st.st_dev as u64,
        ino: st.st_ino as u64,
        is_dir: (mode & libc::S_IFMT) == libc::S_IFDIR,
        is_reg: (mode & libc::S_IFMT) == libc::S_IFREG,
    })
}

/// Open a directory for reading without updating its access time.
///
/// Walking a tree touches every directory, and on a `relatime` mount that is
/// enough to dirty each inode and turn a read-only scan into a stream of
/// metadata writes. `O_NOATIME` needs ownership or `CAP_FOWNER`, and some
/// filesystems reject it outright, so a refusal falls back to a plain open.
///
/// Returns the raw descriptor, or a negative value with `errno` set, matching
/// `libc::open`.
pub fn open_dir_readonly(path: &std::ffi::CStr, follow_links: bool) -> libc::c_int {
    let mut flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC;
    if !follow_links {
        flags |= libc::O_NOFOLLOW;
    }
    let fd = unsafe { libc::open(path.as_ptr(), flags | libc::O_NOATIME) };
    if fd >= 0 {
        return fd;
    }
    match std::io::Error::last_os_error().raw_os_error() {
        Some(libc::EPERM) | Some(libc::EINVAL) => unsafe { libc::open(path.as_ptr(), flags) },
        _ => fd,
    }
}

/// Accumulates a directory's file tally so progress is reported once per
/// directory instead of once per file, and remembers the last accounted file so
/// a sample can be built only when a callback actually fires.
///
pub struct FileCounter {
    files: u64,
    sample: Option<SampleSlot>,
}

struct SampleSlot {
    name: Vec<u8>,
    logical: u64,
    physical: u64,
}

impl FileCounter {
    pub fn new(opt: &crate::Options) -> Self {
        Self {
            files: 0,
            sample: opt.progress_sample_callback.as_ref().map(|_| SampleSlot {
                name: Vec::with_capacity(64),
                logical: 0,
                physical: 0,
            }),
        }
    }

    #[inline]
    pub fn record(&mut self, name: &[u8], logical: u64, physical: u64) {
        self.files += 1;
        if let Some(slot) = self.sample.as_mut() {
            slot.name.clear();
            slot.name.extend_from_slice(name);
            slot.logical = logical;
            slot.physical = physical;
        }
    }

    /// Hand the tally to the shared counter and reset. Called once per
    /// directory, and again whenever a large directory is split.
    pub fn flush(&mut self, ctx: &crate::ScanContext, opt: &crate::Options, dir: &std::path::Path) {
        if self.files == 0 {
            return;
        }
        let files = std::mem::take(&mut self.files);
        ctx.report_progress_batch(opt, files, || match self.sample.as_ref() {
            Some(slot) if !slot.name.is_empty() => {
                use std::{ffi::OsStr, os::unix::ffi::OsStrExt};
                (
                    dir.join(OsStr::from_bytes(&slot.name)),
                    slot.logical,
                    slot.physical,
                )
            }
            _ => (dir.to_path_buf(), 0, 0),
        });
    }
}

/// Pack a `dev_t` the way `statx` reports devices, as `(major << 32) | minor`.
///
/// `fstat` hands back a packed `dev_t` while `statx` splits the device into
/// major and minor fields. Comparing the two encodings directly never matches,
/// so the current directory's device is converted once, here.
#[inline]
pub fn packed_dev(dev: libc::dev_t) -> u64 {
    ((libc::major(dev) as u64) << 32) | (libc::minor(dev) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dirent_decoder_rejects_truncation_invalid_lengths_and_missing_terminators() {
        let mut record = [0u8; 32];
        record[..8].copy_from_slice(&17u64.to_ne_bytes());
        record[8..16].copy_from_slice(&999u64.to_ne_bytes());
        record[16..18].copy_from_slice(&32u16.to_ne_bytes());
        record[18] = libc::DT_REG;
        record[19..22].copy_from_slice(b"abc");
        let entry = decode_dirent(&record).unwrap();
        assert_eq!(
            (entry.ino, entry.offset, entry.kind),
            (17, 999, libc::DT_REG)
        );
        assert_eq!(entry.name.to_bytes(), b"abc");
        for end in 0..32 {
            assert!(decode_dirent(&record[..end]).is_err());
        }
        for length in [0u16, 8, 19, 25, 40] {
            let mut bad = record;
            bad[16..18].copy_from_slice(&length.to_ne_bytes());
            assert!(decode_dirent(&bad).is_err());
        }
        let mut bad = record;
        bad[19..].fill(b'x');
        assert!(decode_dirent(&bad).is_err());
        bad[19] = 0;
        assert!(decode_dirent(&bad).is_err());
        bad = record;
        bad[20] = b'/';
        assert!(decode_dirent(&bad).is_err());
        // Unaligned caller slices are decoded without pointer casts.
        let mut unaligned = vec![0];
        unaligned.extend_from_slice(&record);
        assert_eq!(decode_dirent(&unaligned[1..]).unwrap().ino, 17);
    }

    #[test]
    fn packed_dev_matches_the_statx_encoding() {
        assert_eq!(packed_dev(libc::makedev(8, 17)), (8u64 << 32) | 17);
    }

    #[test]
    fn packed_dev_separates_devices_sharing_a_minor() {
        assert_ne!(
            packed_dev(libc::makedev(8, 1)),
            packed_dev(libc::makedev(9, 1))
        );
    }
}
