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

/// Safe helpers to read fields from a getdents64 dirent buffer
#[inline(always)]
pub unsafe fn dirent_reclen(ptr: *const u8) -> isize {
    *(ptr.add(16) as *const u16) as isize
}

#[inline(always)]
pub unsafe fn dirent_dtype(ptr: *const u8) -> u8 {
    *ptr.add(18)
}

/// getdents64: read d_off field (byte offset 8..15)
#[inline(always)]
pub unsafe fn dirent_d_off(ptr: *const u8) -> u64 {
    *(ptr.add(8) as *const i64) as u64
}

#[inline(always)]
pub unsafe fn dirent_name_len(ptr: *const u8, reclen: isize) -> usize {
    let mut name_len = 0usize;
    while (19 + name_len as isize) < reclen {
        let c = *ptr.add((19 + name_len as isize) as usize);
        if c == 0 {
            break;
        }
        name_len += 1;
    }
    name_len
}

/// Safe slice view over a dirent's name given base pointer and reclen
#[inline(always)]
pub unsafe fn dirent_name_slice<'a>(ptr: *const u8, reclen: isize) -> &'a [u8] {
    let name_len = dirent_name_len(ptr, reclen);
    let name_ptr = ptr.add(19);
    std::slice::from_raw_parts(name_ptr, name_len)
}

/// The dirent's name as a NUL-terminated C string, borrowed from the getdents64
/// buffer itself.
///
/// `d_name` is already NUL-terminated (the kernel pads the record out to
/// `d_reclen`), so a syscall that wants a `*const c_char` can use this pointer
/// directly. Copying the name into a `CString` first cost one malloc and one
/// free per file, and the allocator is the second-largest item in a warm scan
/// profile after `statx` itself.
///
/// # Safety
/// The pointer is only valid until the next `getdents64` overwrites the buffer,
/// so it must be consumed within the same iteration. Anything that outlives the
/// iteration has to copy the bytes.
#[inline(always)]
pub unsafe fn dirent_name_ptr(ptr: *const u8) -> *const libc::c_char {
    ptr.add(19) as *const libc::c_char
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
