//! XFS v5 bulk metadata for an explicitly opted-in, stable read-only filesystem.
//!
//! The caller must keep the superblock read-only for the entire scan and call
//! `is_read_only` before each directory. A transient RW -> RO remount between
//! checks cannot be detected: this is not a snapshot of a mutable filesystem.
//! Only namespace entries discovered by normal traversal may use `lookup`.

use std::{
    fs::{self, File, OpenOptions},
    io,
    os::{
        fd::AsRawFd,
        unix::{ffi::OsStrExt, fs::OpenOptionsExt},
    },
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

const XFS_MAGIC: libc::c_long = 0x5846_5342;
const BATCH_ENTRIES: usize = 256;
const MAX_ENTRIES: usize = 2_000_000;
// Linux x86_64 _IOR('X', 127, struct xfs_bulkstat_req), whose flexible array
// contributes no size. The installed xfs_fs.h defines a 64-byte request header.
const XFS_IOC_BULKSTAT: libc::c_ulong = (2 << 30) | (64 << 16) | (b'X' as u64) << 8 | 127;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Header {
    ino: u64,
    flags: u32,
    icount: u32,
    ocount: u32,
    agno: u32,
    reserved: [u64; 5],
}

// Linux uapi xfs_fs.h, struct xfs_bulkstat version 5. Keep every field and
// reserved slot: bs_mode is at byte 132, not the legacy bulkstat offset.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct BulkStat {
    ino: u64,
    size: u64,
    blocks: u64,
    xflags: u64,
    atime: i64,
    mtime: i64,
    ctime: i64,
    btime: i64,
    generation: u32,
    uid: u32,
    gid: u32,
    projectid: u32,
    atime_nsec: u32,
    mtime_nsec: u32,
    ctime_nsec: u32,
    btime_nsec: u32,
    blksize: u32,
    rdev: u32,
    cowextsize: u32,
    extsize: u32,
    nlink: u32,
    extents: u32,
    aextents: u32,
    version: u16,
    forkoff: u16,
    sick: u16,
    checked: u16,
    mode: u16,
    pad2: u16,
    extents64: u64,
    pad: [u64; 6],
}

#[repr(C)]
struct Batch {
    header: Header,
    records: [BulkStat; BATCH_ENTRIES],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct XfsMetadata {
    pub logical: u64,
    pub blocks512: u64,
    pub dev: u64,
    pub ino: u64,
    pub nlink: u32,
    pub mode: u32,
}

/// Immutable scan-local cache; callers must honor the stable read-only contract.
#[derive(Debug)]
pub struct XfsCache {
    root: File,
    dev: u64,
    mount_id: u64,
    canonical: PathBuf,
    entries: Vec<XfsMetadata>,
    invalidated: AtomicBool,
}

impl XfsCache {
    /// `None` means ineligible mount or unavailable ioctl/permission. Other I/O,
    /// corrupt output, resource-limit and cancellation failures return `Err`.
    /// Every failure discards the complete candidate; no partial cache escapes.
    pub(crate) fn prepare(root: &Path, cancel: &AtomicBool) -> io::Result<Option<Self>> {
        check_cancel(cancel)?;
        let canonical = fs::canonicalize(root)?;
        let root = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&canonical)?;
        if !read_only_xfs(&root)? {
            return Ok(None);
        }
        let Some((dev, mount_id)) = descriptor_identity(&root)? else {
            return Ok(None);
        };
        if !eligible_mount(
            &fs::read("/proc/self/mountinfo")?,
            &canonical,
            dev,
            mount_id,
        ) {
            return Ok(None);
        }
        // XFS_IOC_BULKSTAT enforces CAP_SYS_ADMIN. Do not infer capability from
        // uid or request elevation; denial is an ordinary capability fallback.
        let entries = match collect(dev, cancel, MAX_ENTRIES, |batch| {
            // SAFETY: Batch has the C header followed immediately by icount
            // writable records. The synchronous ioctl cannot retain this pointer.
            let result =
                unsafe { libc::ioctl(root.as_raw_fd(), XFS_IOC_BULKSTAT, batch as *mut Batch) };
            if result < 0 {
                Err(io::Error::last_os_error())
            } else if result != 0 {
                Err(invalid("unexpected XFS bulkstat ioctl return value"))
            } else {
                Ok(())
            }
        }) {
            Ok(entries) => entries,
            Err(err) if capability_error(&err) => return Ok(None),
            Err(err) => return Err(err),
        };
        check_cancel(cancel)?;
        // Detect an observed remount or namespace replacement during preparation.
        if !read_only_xfs(&root)?
            || !eligible_mount(
                &fs::read("/proc/self/mountinfo")?,
                &canonical,
                dev,
                mount_id,
            )
        {
            return Ok(None);
        }
        Ok(Some(Self {
            root,
            dev,
            mount_id,
            canonical,
            entries,
            invalidated: AtomicBool::new(false),
        }))
    }

    /// Check once per directory before using cached metadata. Any observed
    /// writable/error state invalidates this cache for the remainder of the scan.
    pub(crate) fn is_read_only(&self) -> bool {
        if self.invalidated.load(Ordering::Acquire) {
            return false;
        }
        let eligible = read_only_xfs(&self.root).unwrap_or(false)
            && fs::read("/proc/self/mountinfo")
                .is_ok_and(|info| eligible_mount(&info, &self.canonical, self.dev, self.mount_id));
        if !eligible {
            self.invalidated.store(true, Ordering::Release);
            return false;
        }
        !self.invalidated.load(Ordering::Acquire)
    }

    /// No per-file syscall. `dev` is major << 32 | minor, as in Linux statx.
    /// A submount, missing inode or invalidated cache requires normal metadata.
    pub(crate) fn lookup(&self, dev: u64, ino: u64) -> Option<XfsMetadata> {
        if self.invalidated.load(Ordering::Acquire) {
            return None;
        }
        find_entry(&self.entries, self.dev, dev, ino)
    }
}

fn find_entry(entries: &[XfsMetadata], cache_dev: u64, dev: u64, ino: u64) -> Option<XfsMetadata> {
    if dev != cache_dev {
        return None;
    }
    entries
        .binary_search_by_key(&ino, |entry| entry.ino)
        .ok()
        .map(|index| entries[index])
}

fn read_only_xfs(file: &File) -> io::Result<bool> {
    // SAFETY: statfs contains only integers; zero initialization is valid.
    let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: fd remains open, and stat points to valid writable storage.
    if unsafe { libc::fstatfs(file.as_raw_fd(), &mut stat) } != 0 {
        return Err(io::Error::last_os_error());
    }
    if stat.f_type != XFS_MAGIC {
        return Ok(false);
    }
    // libc keeps statfs.f_flags private on some supported versions. statvfs
    // exposes the mount flags; mountinfo separately checks the superblock flag.
    // SAFETY: statvfs is integer-only and the descriptor stays open.
    let mut flags: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstatvfs(file.as_raw_fd(), &mut flags) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(flags.f_flag & libc::ST_RDONLY != 0)
}

fn descriptor_identity(file: &File) -> io::Result<Option<(u64, u64)>> {
    // SAFETY: statx is a C integer-only output structure.
    let mut stat: libc::statx = unsafe { std::mem::zeroed() };
    // SAFETY: AT_EMPTY_PATH selects the held descriptor, the name is terminated,
    // and stat is live writable memory. Require mount identity, never guess it.
    if unsafe {
        libc::statx(
            file.as_raw_fd(),
            b"\0".as_ptr().cast(),
            libc::AT_EMPTY_PATH | libc::AT_SYMLINK_NOFOLLOW,
            libc::STATX_MNT_ID,
            &mut stat,
        )
    } != 0
    {
        let err = io::Error::last_os_error();
        return if capability_error(&err) {
            Ok(None)
        } else {
            Err(err)
        };
    }
    if stat.stx_mask & libc::STATX_MNT_ID == 0 {
        return Ok(None);
    }
    Ok(Some((
        (u64::from(stat.stx_dev_major) << 32) | u64::from(stat.stx_dev_minor),
        stat.stx_mnt_id,
    )))
}

fn eligible_mount(info: &[u8], canonical: &Path, dev: u64, mount_id: u64) -> bool {
    for line in info.split(|byte| *byte == b'\n') {
        let fields: Vec<_> = line.split(|byte| *byte == b' ').collect();
        if fields.len() < 10 || decimal(fields[0]) != Some(mount_id) {
            continue;
        }
        let Some(separator) = fields.iter().position(|field| *field == b"-") else {
            return false;
        };
        if separator < 6 || fields.len() != separator + 4 {
            return false;
        }
        let mut parts = fields[2].split(|byte| *byte == b':');
        let pair = parts
            .next()
            .and_then(decimal)
            .zip(parts.next().and_then(decimal));
        let Some((major, minor)) = pair else {
            return false;
        };
        if parts.next().is_some() || major > u32::MAX as u64 || minor > u32::MAX as u64 {
            return false;
        }
        let options: Vec<_> = fields[separator + 3].split(|byte| *byte == b',').collect();
        return (major << 32) | minor == dev
            && fields[3] == b"/"
            && unescape_mount_path(fields[4]).as_deref() == Some(canonical.as_os_str().as_bytes())
            && fields[separator + 1] == b"xfs"
            && options.contains(&b"ro".as_slice())
            && !options.contains(&b"rw".as_slice());
    }
    false
}

fn decimal(bytes: &[u8]) -> Option<u64> {
    if bytes.is_empty() {
        return None;
    }
    bytes.iter().try_fold(0u64, |value, byte| {
        if !byte.is_ascii_digit() {
            return None;
        }
        value.checked_mul(10)?.checked_add(u64::from(byte - b'0'))
    })
}

fn unescape_mount_path(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut path = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\\' {
            let escaped = bytes.get(index + 1..index + 4)?;
            path.push(match escaped {
                b"040" => b' ',
                b"011" => b'\t',
                b"012" => b'\n',
                b"134" => b'\\',
                _ => return None,
            });
            index += 4;
        } else {
            path.push(bytes[index]);
            index += 1;
        }
    }
    Some(path)
}

fn collect(
    dev: u64,
    cancel: &AtomicBool,
    max_entries: usize,
    mut next: impl FnMut(&mut Batch) -> io::Result<()>,
) -> io::Result<Vec<XfsMetadata>> {
    let mut entries: Vec<XfsMetadata> = Vec::new();
    let mut cursor = 0;
    let mut last = None;
    loop {
        check_cancel(cancel)?;
        let mut batch = Batch {
            header: Header {
                ino: cursor,
                icount: BATCH_ENTRIES as u32,
                ..Header::default()
            },
            records: [BulkStat::default(); BATCH_ENTRIES],
        };
        match next(&mut batch) {
            Err(err) if err.raw_os_error() == Some(libc::EINTR) => continue,
            Err(err) => return Err(err),
            Ok(()) => {}
        }
        check_cancel(cancel)?;
        let count = batch.header.ocount as usize;
        if count > BATCH_ENTRIES
            || batch.header.icount != BATCH_ENTRIES as u32
            || batch.header.flags != 0
            || batch.header.agno != 0
            || batch.header.reserved != [0; 5]
            || batch.header.ino < cursor
        {
            return Err(invalid("malformed XFS bulkstat output header"));
        }
        if count == 0 {
            return Ok(entries);
        }
        let new_len = entries
            .len()
            .checked_add(count)
            .filter(|length| *length <= max_entries)
            .ok_or_else(|| io::Error::other("XFS cache entry limit exceeded"))?;
        if new_len > entries.capacity() {
            let capacity = new_len
                .checked_next_power_of_two()
                .unwrap_or(max_entries)
                .min(max_entries);
            entries
                .try_reserve_exact(capacity - entries.len())
                .map_err(|_| io::Error::other("XFS cache allocation failed"))?;
        }
        for record in &batch.records[..count] {
            check_cancel(cancel)?;
            if record.ino < cursor || last.is_some_and(|ino| record.ino <= ino) {
                return Err(invalid("XFS bulkstat inode sequence did not advance"));
            }
            let metadata = convert(record, dev)?;
            last = Some(record.ino);
            entries.push(metadata);
        }
        if batch.header.ino <= last.unwrap_or(cursor) {
            return Err(invalid(
                "XFS bulkstat cursor did not advance past returned inodes",
            ));
        }
        cursor = batch.header.ino;
    }
}

fn convert(record: &BulkStat, dev: u64) -> io::Result<XfsMetadata> {
    if record.version != 5
        || record.ino == 0
        || record.size > i64::MAX as u64
        || record.sick != 0
        || record.blksize < 512
        || !record.blksize.is_power_of_two()
    {
        return Err(invalid("invalid XFS v5 bulkstat metadata"));
    }
    if !matches!(
        u32::from(record.mode) & libc::S_IFMT,
        libc::S_IFREG
            | libc::S_IFDIR
            | libc::S_IFLNK
            | libc::S_IFIFO
            | libc::S_IFSOCK
            | libc::S_IFCHR
            | libc::S_IFBLK
    ) {
        return Err(invalid("unknown XFS inode type"));
    }
    // bs_blocks uses filesystem blocks (bs_blksize), unlike stat.st_blocks.
    let physical = record
        .blocks
        .checked_mul(u64::from(record.blksize))
        .ok_or_else(|| invalid("XFS allocated byte count overflow"))?;
    Ok(XfsMetadata {
        logical: record.size,
        blocks512: physical / 512,
        dev,
        ino: record.ino,
        nlink: record.nlink,
        mode: u32::from(record.mode),
    })
}

fn check_cancel(cancel: &AtomicBool) -> io::Result<()> {
    if cancel.load(Ordering::Relaxed) {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "XFS cache preparation cancelled",
        ))
    } else {
        Ok(())
    }
}

fn capability_error(error: &io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(
            libc::ENOTTY
                | libc::ENOSYS
                | libc::EOPNOTSUPP
                | libc::EINVAL
                | libc::EPERM
                | libc::EACCES
        )
    )
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(ino: u64) -> BulkStat {
        BulkStat {
            ino,
            version: 5,
            size: 8192,
            blocks: 2,
            blksize: 4096,
            nlink: 1,
            mode: libc::S_IFREG as u16 | 0o600,
            ..BulkStat::default()
        }
    }

    fn reply(batch: &mut Batch, records: &[BulkStat], cursor: u64) {
        batch.header.ocount = records.len() as u32;
        batch.header.ino = cursor;
        batch.records[..records.len()].copy_from_slice(records);
    }

    #[test]
    fn native_owned_xfs_cache_matches_namespace_stat() {
        use std::os::unix::fs::MetadataExt;
        let Some(path) = std::env::var_os("HYPERDU_TEST_XFS_ROOT") else {
            eprintln!("owned XFS fixture not supplied; native ioctl not verified");
            return;
        };
        let root = PathBuf::from(path);
        let cache = XfsCache::prepare(&root, &AtomicBool::new(false))
            .expect("native XFS preparation")
            .expect("owned read-only XFS fixture must use the real bulkstat ioctl");
        assert!(cache.is_read_only());
        let mut directories = vec![root.clone()];
        let mut checked = 0;
        while let Some(directory) = directories.pop() {
            for entry in fs::read_dir(directory).unwrap() {
                let entry = entry.unwrap();
                let stat = fs::symlink_metadata(entry.path()).unwrap();
                let dev = crate::platform::linux_helpers::packed_dev(stat.dev());
                let value = cache
                    .lookup(dev, stat.ino())
                    .expect("namespace inode in bulk cache");
                assert_eq!(
                    (value.logical, value.blocks512, value.nlink, value.mode),
                    (stat.len(), stat.blocks(), stat.nlink() as u32, stat.mode()),
                    "{}",
                    entry.path().display()
                );
                assert!(cache.lookup(dev ^ (1 << 32), stat.ino()).is_none());
                checked += 1;
                if stat.is_dir() {
                    directories.push(entry.path());
                }
            }
        }
        assert!(
            checked > 64,
            "fixture must exercise multiple metadata batches"
        );
        assert!(
            XfsCache::prepare(&root.join("nested"), &AtomicBool::new(false))
                .unwrap()
                .is_none(),
            "partial namespace roots must not enumerate every inode"
        );
        eprintln!("native XFS bulkstat verified against {checked} namespace entries");
    }
    #[test]
    fn native_abi_sizes_alignment_and_offsets() {
        assert_eq!(std::mem::size_of::<Header>(), 64);
        assert_eq!(std::mem::align_of::<Header>(), 8);
        assert_eq!(std::mem::size_of::<BulkStat>(), 192);
        assert_eq!(std::mem::align_of::<BulkStat>(), 8);
        assert_eq!(std::mem::size_of::<Batch>(), 64 + 192 * BATCH_ENTRIES);
        assert_eq!(XFS_IOC_BULKSTAT, 0x8040_587f);
        // Pointer subtraction within the same live object avoids requiring
        // offset_of!, which is newer than this crate's Rust 1.75 MSRV.
        let value = BulkStat::default();
        let base = &value as *const BulkStat as usize;
        assert_eq!(&value.blksize as *const u32 as usize - base, 96);
        assert_eq!(&value.nlink as *const u32 as usize - base, 112);
        assert_eq!(&value.version as *const u16 as usize - base, 124);
        assert_eq!(&value.mode as *const u16 as usize - base, 132);
        assert_eq!(&value.extents64 as *const u64 as usize - base, 136);
    }

    #[test]
    fn filesystem_block_units_and_sparse_zero_are_preserved() {
        let mut input = record(9);
        let output = convert(&input, 7).unwrap();
        assert_eq!(output.blocks512, 16);
        assert_eq!(
            (output.dev, output.ino, output.nlink, output.logical),
            (7, 9, 1, 8192)
        );
        input.blocks = 0;
        assert_eq!(convert(&input, 7).unwrap().blocks512, 0);
        assert_eq!(convert(&input, 7).unwrap().logical, 8192);
        input.blocks = u64::MAX;
        assert_eq!(
            convert(&input, 7).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn malformed_metadata_is_rejected() {
        for input in [
            BulkStat {
                version: 1,
                ..record(1)
            },
            BulkStat {
                ino: 0,
                ..record(1)
            },
            BulkStat {
                size: u64::MAX,
                ..record(1)
            },
            BulkStat {
                blksize: 0,
                ..record(1)
            },
            BulkStat {
                blksize: 768,
                ..record(1)
            },
            BulkStat {
                sick: 1,
                ..record(1)
            },
            BulkStat {
                mode: 0,
                ..record(1)
            },
        ] {
            assert_eq!(
                convert(&input, 7).unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
        }
    }

    #[test]
    fn sorted_pages_complete_only_at_eof_and_lookup_is_device_scoped() {
        let mut calls = 0;
        let entries = collect(7, &AtomicBool::new(false), 3, |batch| {
            match calls {
                0 => {
                    assert_eq!(batch.header.ino, 0);
                    reply(batch, &[record(1), record(9)], 10);
                }
                1 => {
                    assert_eq!(batch.header.ino, 10);
                    reply(batch, &[record(10)], 11);
                }
                2 => assert_eq!(batch.header.ino, 11),
                _ => panic!("read past EOF"),
            }
            calls += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(calls, 3);
        assert_eq!(entries.len(), 3);
        assert_eq!(find_entry(&entries, 7, 7, 9).unwrap().ino, 9);
        assert!(find_entry(&entries, 7, 8, 9).is_none());
        assert!(find_entry(&entries, 7, 7, 8).is_none());
    }

    #[test]
    fn header_count_and_reserved_fields_are_validated() {
        for invalid_case in 0..5 {
            let result = collect(7, &AtomicBool::new(false), 10, |batch| {
                match invalid_case {
                    0 => batch.header.ocount = BATCH_ENTRIES as u32 + 1,
                    1 => batch.header.icount = 1,
                    2 => batch.header.flags = 1,
                    3 => batch.header.agno = 1,
                    4 => batch.header.reserved[0] = 1,
                    _ => unreachable!(),
                }
                Ok(())
            });
            assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
        }
    }

    #[test]
    fn repeated_inodes_backward_inodes_and_cursor_are_rejected() {
        for (inodes, cursor) in [(vec![1, 1], 2), (vec![2, 1], 3), (vec![2], 2)] {
            let inputs: Vec<_> = inodes.into_iter().map(record).collect();
            let result = collect(7, &AtomicBool::new(false), 10, |batch| {
                reply(batch, &inputs, cursor);
                Ok(())
            });
            assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
        }
        for eof in [false, true] {
            let mut calls = 0;
            let result = collect(7, &AtomicBool::new(false), 10, |batch| {
                if calls == 0 {
                    reply(batch, &[record(9)], 10);
                } else if eof {
                    batch.header.ino = 9;
                } else {
                    reply(batch, &[record(9)], 11);
                }
                calls += 1;
                Ok(())
            });
            assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
            assert_eq!(calls, 2);
        }
    }

    #[test]
    fn limit_and_later_io_error_never_publish_partial_cache() {
        let result = collect(7, &AtomicBool::new(false), 1, |batch| {
            reply(batch, &[record(1), record(2)], 3);
            Ok(())
        });
        assert!(result.unwrap_err().to_string().contains("limit"));
        let mut calls = 0;
        let result = collect(7, &AtomicBool::new(false), 10, |batch| {
            calls += 1;
            if calls == 1 {
                reply(batch, &[record(1)], 2);
                Ok(())
            } else {
                Err(io::Error::from_raw_os_error(libc::EIO))
            }
        });
        assert_eq!(result.unwrap_err().raw_os_error(), Some(libc::EIO));
    }

    #[test]
    fn cancellation_before_and_during_collection_returns_no_cache() {
        let cancelled = AtomicBool::new(true);
        assert_eq!(
            collect(7, &cancelled, 10, |_| panic!("cancelled syscall"))
                .unwrap_err()
                .kind(),
            io::ErrorKind::Interrupted
        );
        cancelled.store(false, Ordering::Relaxed);
        let result = collect(7, &cancelled, 10, |batch| {
            reply(batch, &[record(1)], 2);
            cancelled.store(true, Ordering::Relaxed);
            Ok(())
        });
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Interrupted);
    }

    #[test]
    fn interrupted_ioctl_retries_same_cursor_and_rechecks_cancellation() {
        let mut calls = 0;
        let entries = collect(7, &AtomicBool::new(false), 10, |batch| {
            assert_eq!(batch.header.ino, 0);
            calls += 1;
            if calls == 1 {
                Err(io::Error::from_raw_os_error(libc::EINTR))
            } else {
                Ok(())
            }
        })
        .unwrap();
        assert_eq!(calls, 2);
        assert!(entries.is_empty());
        let cancel = AtomicBool::new(false);
        let result = collect(7, &cancel, 10, |_| {
            cancel.store(true, Ordering::Relaxed);
            Err(io::Error::from_raw_os_error(libc::EINTR))
        });
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Interrupted);
    }

    fn mount(root: &str, point: &str, options: &str, super_options: &str) -> Vec<u8> {
        format!("31 20 8:1 {root} {point} {options} shared:4 - xfs /dev/test {super_options}\n")
            .into_bytes()
    }

    #[test]
    fn mount_gate_requires_exact_id_device_root_and_read_only_superblock() {
        let dev = (8u64 << 32) | 1;
        let path = Path::new("/mnt/data");
        let valid = mount("/", "/mnt/data", "ro,relatime", "ro,attr2");
        assert!(eligible_mount(&valid, path, dev, 31));
        assert!(!eligible_mount(&valid, path, dev, 32));
        assert!(!eligible_mount(&valid, path, dev + 1, 31));
        assert!(!eligible_mount(&valid, Path::new("/mnt/data/sub"), dev, 31));
        assert!(!eligible_mount(&valid, Path::new("/mnt/dat"), dev, 31));
        // A read-only bind cannot make a writable superblock stable.
        assert!(!eligible_mount(
            &mount("/", "/mnt/data", "ro", "rw,attr2"),
            path,
            dev,
            31
        ));
        assert!(!eligible_mount(
            &mount("/sub", "/mnt/data", "ro", "ro"),
            path,
            dev,
            31
        ));
        assert!(!eligible_mount(
            &mount("/", "/mnt/data", "ro", "errors=ro"),
            path,
            dev,
            31
        ));
        assert!(!eligible_mount(
            &mount("/", "/mnt/data", "ro", "ro,rw"),
            path,
            dev,
            31
        ));
    }

    #[test]
    fn mount_paths_decode_only_kernel_escapes() {
        let dev = (8u64 << 32) | 1;
        let info = mount("/", r"/mnt/a\040b\134c\011d\012e", "ro", "ro");
        assert!(eligible_mount(
            &info,
            Path::new("/mnt/a b\\c\td\ne"),
            dev,
            31
        ));
        for bad in [r"/mnt/\", r"/mnt/\04", r"/mnt/\000", r"/mnt/\777"] {
            assert!(unescape_mount_path(bad.as_bytes()).is_none());
        }
        assert_eq!(unescape_mount_path(b"/mnt/\xff").unwrap(), b"/mnt/\xff");
        assert!(decimal(b"18446744073709551616").is_none());
        assert!(decimal(b"+31").is_none());
    }

    #[test]
    fn malformed_and_non_xfs_mount_rows_fail_closed() {
        let dev = (8u64 << 32) | 1;
        for row in [
            "31 20 8:1 / /mnt/data ro - ext4 /dev/test ro",
            "31 20 8:1 / /mnt/data ro xfs /dev/test ro",
            "31 20 8:1:2 / /mnt/data ro - xfs /dev/test ro",
            "31 20 4294967296:1 / /mnt/data ro - xfs /dev/test ro",
            "31 20 8:1 / /mnt/data ro - xfs /dev/test",
            "31 20 8:1 / /mnt/data ro - xfs /dev/test ro garbage",
        ] {
            assert!(
                !eligible_mount(row.as_bytes(), Path::new("/mnt/data"), dev, 31),
                "{row}"
            );
        }
    }

    #[test]
    fn capability_errors_differ_from_corruption_and_real_io_errors() {
        for errno in [
            libc::EPERM,
            libc::EACCES,
            libc::ENOTTY,
            libc::ENOSYS,
            libc::EINVAL,
            libc::EOPNOTSUPP,
        ] {
            assert!(capability_error(&io::Error::from_raw_os_error(errno)));
        }
        assert!(!capability_error(&io::Error::from_raw_os_error(libc::EIO)));
        assert!(!capability_error(&invalid("malformed result")));
        assert!(
            XfsCache::prepare(Path::new("/proc"), &AtomicBool::new(false))
                .unwrap()
                .is_none()
        );
        assert_eq!(
            XfsCache::prepare(Path::new("/nonexistent"), &AtomicBool::new(true))
                .unwrap_err()
                .kind(),
            io::ErrorKind::Interrupted
        );
    }

    #[test]
    fn observed_ineligible_filesystem_invalidates_cache_permanently() {
        let cache = XfsCache {
            root: File::open("/proc").unwrap(),
            dev: 7,
            mount_id: 31,
            canonical: Path::new("/proc").into(),
            entries: vec![convert(&record(1), 7).unwrap()],
            invalidated: AtomicBool::new(false),
        };
        assert!(cache.lookup(7, 1).is_some());
        assert!(!cache.is_read_only());
        assert!(cache.invalidated.load(Ordering::Relaxed));
        assert!(cache.lookup(7, 1).is_none());
        assert!(!cache.is_read_only());
    }
}
