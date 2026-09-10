use std::{
    ffi::{CStr, CString, OsStr},
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::ffi::OsStrExt,
    },
    sync::atomic::Ordering,
};

use crate::{
    common_ops::{
        check_hardlink_duplicate, check_visited_directory, hardlink_candidate, report_files_batch,
        update_file_stats,
    },
    error_handling::{last_os_error_systemcall, record_error, ScanError},
    DirContext, Options, ScanContext, Stat, StatMap,
};

mod bulk;
use bulk::{parse_record, u32_at, Entry, COMMON, FILE, VBLK, VCHR, VDIR, VFIFO, VLNK, VREG, VSOCK};

// Match file_stat and Darwin stat: logical is the data fork, while physical
// includes all forks and rounds allocation to 512 bytes (xnu vfs_vnops.c).
// TOTALSIZE sums fork lengths and is intentionally not requested.
struct Metadata {
    kind: u32,
    device: u64,
    inode: u64,
    links: u32,
    logical: u64,
    physical: u64,
}

impl Entry<'_> {
    fn metadata(&self, opt: &Options) -> Option<Metadata> {
        let kind = self.kind?;
        // GALB describes a symlink itself and may describe the directory under
        // a mount point. stat the traversal target for -L and -x directories.
        if kind == 0
            || kind > 7
            || self.error != 0
            || (kind == VLNK && opt.follow_links)
            || (kind == VDIR && (opt.one_file_system || opt.follow_links))
        {
            return None;
        }
        let need_identity = kind != VDIR && !opt.count_hardlinks && opt.inode_cache.is_some();
        Some(Metadata {
            kind,
            device: if need_identity {
                self.device?
            } else {
                self.device.unwrap_or(0)
            },
            inode: if need_identity {
                self.inode?
            } else {
                self.inode.unwrap_or(0)
            },
            links: if need_identity {
                self.links?
            } else {
                self.links.unwrap_or(1)
            },
            logical: if kind == VDIR { 0 } else { self.logical? },
            physical: if kind == VDIR || !opt.compute_physical {
                0
            } else {
                self.physical?.div_ceil(512) * 512
            },
        })
    }
}

fn metadata_at(fd: libc::c_int, name: &CStr, follow: bool) -> std::io::Result<Metadata> {
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    let flags = if follow { 0 } else { libc::AT_SYMLINK_NOFOLLOW };
    if unsafe { libc::fstatat(fd, name.as_ptr(), &mut st, flags) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(Metadata {
        kind: match st.st_mode & libc::S_IFMT {
            libc::S_IFDIR => VDIR,
            libc::S_IFLNK => VLNK,
            libc::S_IFREG => VREG,
            libc::S_IFBLK => VBLK,
            libc::S_IFCHR => VCHR,
            libc::S_IFSOCK => VSOCK,
            libc::S_IFIFO => VFIFO,
            _ => 0,
        },
        device: st.st_dev as u64,
        inode: st.st_ino,
        links: u32::from(st.st_nlink),
        logical: u64::try_from(st.st_size).map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "negative stat size")
        })?,
        physical: u64::try_from(st.st_blocks)
            .map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, "negative stat blocks")
            })?
            .saturating_mul(512),
    })
}

fn handle_entry(
    ctx: &ScanContext,
    dctx: &DirContext,
    fd: libc::c_int,
    entry: &Entry<'_>,
    stat: &mut Stat,
) -> Option<(u64, u64)> {
    let opt = ctx.options;
    let name = entry.name?;
    let bytes = name.to_bytes();
    if bytes == b"." || bytes == b".." || crate::name_matches(bytes, opt) {
        return None;
    }
    // No PathBuf in the ordinary file path. Filters, directories, diagnostics
    // and the lazily evaluated progress sample are the only consumers.
    let child = || dctx.dir.join(OsStr::from_bytes(bytes));
    if opt.needs_path_filter && crate::path_excluded(&child(), opt) {
        return None;
    }
    if entry.kind == Some(VLNK) && !opt.follow_links {
        return None;
    }
    if entry.error != 0 {
        record_error(
            opt,
            &ScanError::SystemCall {
                path: child(),
                call: "getattrlistbulk entry",
                errno: entry.error as i32,
            },
        );
    }
    let md = match entry.metadata(opt) {
        Some(md) => md,
        None => match metadata_at(fd, name, opt.follow_links) {
            Ok(md) => md,
            Err(source) => {
                record_error(
                    opt,
                    &ScanError::IoError {
                        path: child(),
                        source,
                    },
                );
                return None;
            }
        },
    };
    if md.kind == VLNK && !opt.follow_links {
        return None;
    }
    if md.kind == VDIR {
        if (opt.max_depth == 0 || dctx.depth < opt.max_depth)
            && !(opt.one_file_system && opt.root_fs_id != 0 && md.device != opt.root_fs_id)
            && !check_visited_directory(opt, md.device, md.inode)
            && !opt.cancel.load(Ordering::Relaxed)
        {
            ctx.enqueue_dir(child(), dctx.depth + 1);
        }
        return None;
    }
    if md.kind != VREG || md.logical < opt.min_file_size {
        return None;
    }
    if hardlink_candidate(opt, md.links) && check_hardlink_duplicate(opt, md.device, md.inode) {
        return None;
    }
    let physical = if opt.compute_physical {
        md.physical
    } else {
        md.logical
    };
    update_file_stats(stat, md.logical, physical);
    Some((md.logical, physical))
}

fn fallback_directory(ctx: &ScanContext, dctx: &DirContext, fd: libc::c_int, stat: &mut Stat) {
    // Never mix readdir with GALB on the same fd. Only entered before GALB has
    // returned any entries, so a fresh enumeration cannot double-count files.
    let entries = match std::fs::read_dir(dctx.dir) {
        Ok(entries) => entries,
        Err(source) => {
            record_error(
                ctx.options,
                &ScanError::IoError {
                    path: dctx.dir.to_path_buf(),
                    source,
                },
            );
            return;
        }
    };
    for item in entries {
        if ctx.options.cancel.load(Ordering::Relaxed) {
            break;
        }
        let item = match item {
            Ok(item) => item,
            Err(source) => {
                record_error(
                    ctx.options,
                    &ScanError::IoError {
                        path: dctx.dir.to_path_buf(),
                        source,
                    },
                );
                continue;
            }
        };
        let name = item.file_name();
        let Ok(name) = CString::new(name.as_bytes()) else {
            record_error(ctx.options, &ScanError::InvalidPath { path: item.path() });
            continue;
        };
        let entry = Entry {
            name: Some(&name),
            ..Entry::default()
        };
        if let Some((logical, physical)) = handle_entry(ctx, dctx, fd, &entry, stat) {
            report_files_batch(ctx.options, ctx.total_files, 1, || {
                (
                    dctx.dir.join(OsStr::from_bytes(name.to_bytes())),
                    logical,
                    physical,
                )
            });
        }
    }
}

pub fn process_dir(ctx: &ScanContext, dctx: &DirContext, map: &mut StatMap) {
    // Diagnostic/reference path: exact "0" disables GALB.
    let use_bulk = !std::env::var_os("HYPERDU_MAC_USE_GALB").is_some_and(|v| v == "0");
    process_dir_with(ctx, dctx, map, use_bulk, read_bulk_records);
}

fn read_bulk_records(
    fd: libc::c_int,
    attrs: &mut libc::attrlist,
    buffer: &mut [u8],
) -> std::io::Result<usize> {
    let n = unsafe {
        libc::getattrlistbulk(
            fd,
            (attrs as *mut libc::attrlist).cast(),
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            u64::from(libc::FSOPT_PACK_INVAL_ATTRS),
        )
    };
    if n < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(n as usize)
    }
}

// Inject the syscall at this boundary so native tests exercise real fallback
// enumeration and error reporting without process-global test overrides.
fn process_dir_with(
    ctx: &ScanContext,
    dctx: &DirContext,
    map: &mut StatMap,
    use_bulk: bool,
    mut read_bulk: impl FnMut(libc::c_int, &mut libc::attrlist, &mut [u8]) -> std::io::Result<usize>,
) {
    let opt = ctx.options;
    if opt.cancel.load(Ordering::Relaxed) {
        return;
    }
    let c_path = match CString::new(dctx.dir.as_os_str().as_bytes()) {
        Ok(path) => path,
        Err(_) => {
            record_error(
                opt,
                &ScanError::InvalidPath {
                    path: dctx.dir.to_path_buf(),
                },
            );
            return;
        }
    };
    let nofollow = if opt.follow_links {
        0
    } else {
        libc::O_NOFOLLOW
    };
    let raw = unsafe {
        libc::open(
            c_path.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | nofollow,
        )
    };
    if raw < 0 {
        record_error(opt, &last_os_error_systemcall(dctx.dir, "open"));
        return;
    }
    // Own the descriptor through every early return and callback unwind.
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    if opt.one_file_system || (crate::follows_links(opt) && dctx.depth == 0) {
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstat(fd.as_raw_fd(), &mut st) } != 0 {
            record_error(opt, &last_os_error_systemcall(dctx.dir, "fstat directory"));
            return;
        }
        if opt.one_file_system && opt.root_fs_id != 0 && st.st_dev as u64 != opt.root_fs_id {
            return;
        }
        if opt.follow_links && dctx.depth == 0 {
            check_visited_directory(opt, st.st_dev as u64, st.st_ino);
        }
    }
    let stat = map.entry(dctx.dir.to_path_buf()).or_default();
    if !use_bulk {
        fallback_directory(ctx, dctx, fd.as_raw_fd(), stat);
        return;
    }
    let mut attrs = libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: COMMON,
        volattr: 0,
        dirattr: 0,
        fileattr: FILE,
        forkattr: 0,
    };
    // Bound tuning input to avoid arithmetic overflow or unbounded allocation.
    let kb = std::env::var("HYPERDU_GALB_BUF_KB")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(64)
        .clamp(4, 4096);
    let mut buffer = vec![0u8; kb * 1024];
    let mut started = false;
    while !opt.cancel.load(Ordering::Relaxed) {
        #[cfg(any(feature = "prof-tracy", feature = "prof-puffin"))]
        profiling::scope!("getattrlistbulk_loop");
        let n = match read_bulk(fd.as_raw_fd(), &mut attrs, &mut buffer) {
            Ok(0) => break,
            Ok(n) => n,
            Err(err) => {
                if err.raw_os_error() == Some(libc::EINTR) {
                    continue;
                }
                let unsupported = matches!(
                    err.raw_os_error(),
                    Some(libc::ENOTSUP | libc::ENOSYS | libc::EINVAL)
                );
                if !started && unsupported {
                    // Capability negotiation is successful when the fallback
                    // succeeds. Its actual I/O failures are reported there.
                    fallback_directory(ctx, dctx, fd.as_raw_fd(), stat);
                } else {
                    record_error(
                        opt,
                        &ScanError::IoError {
                            path: dctx.dir.to_path_buf(),
                            source: err,
                        },
                    );
                }
                break;
            }
        };
        started = true;
        let mut offset = 0;
        let mut count = 0;
        let mut sample = None;
        for _ in 0..n {
            if opt.cancel.load(Ordering::Relaxed) {
                break;
            }
            let remaining = &buffer[offset..];
            let len = u32_at(remaining, 0).unwrap_or(0) as usize;
            if len < 4 || len > remaining.len() {
                record_error(
                    opt,
                    &ScanError::IoError {
                        path: dctx.dir.to_path_buf(),
                        source: std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "invalid getattrlistbulk record length",
                        ),
                    },
                );
                break;
            }
            match parse_record(&remaining[..len]) {
                Ok(entry) => {
                    if let Some((logical, physical)) =
                        handle_entry(ctx, dctx, fd.as_raw_fd(), &entry, stat)
                    {
                        count += 1;
                        sample = entry.name.map(|name| (name, logical, physical));
                    }
                }
                Err(message) => record_error(
                    opt,
                    &ScanError::IoError {
                        path: dctx.dir.to_path_buf(),
                        source: std::io::Error::new(std::io::ErrorKind::InvalidData, message),
                    },
                ),
            }
            offset += len;
        }
        if let Some((name, logical, physical)) = sample {
            report_files_batch(opt, ctx.total_files, count, || {
                (
                    dctx.dir.join(OsStr::from_bytes(name.to_bytes())),
                    logical,
                    physical,
                )
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan_with(
        root: &std::path::Path,
        opt: &Options,
        read_bulk: impl FnMut(libc::c_int, &mut libc::attrlist, &mut [u8]) -> std::io::Result<usize>,
    ) -> Stat {
        // The shared progress counter is intentionally idle when progress is
        // disabled. Enable it because this fixture asserts counter/stat parity.
        let opt = Options {
            progress_every: 1,
            ..opt.clone()
        };
        let workers = crate::scheduler::Scheduler::make_workers(1);
        let sched = crate::scheduler::Scheduler::new(&workers);
        let total_files = std::sync::atomic::AtomicU64::new(0);
        let ctx = ScanContext {
            options: &opt,
            sched: &sched,
            local: &workers[0],
            total_files: &total_files,
            deferred: None,
        };
        let dctx = DirContext {
            dir: root,
            depth: 0,
            resume: None,
        };
        let mut map = StatMap::default();
        process_dir_with(&ctx, &dctx, &mut map, true, read_bulk);
        let stat = *map.get(root).unwrap();
        assert_eq!(total_files.load(Ordering::Relaxed), stat.files);
        stat
    }

    #[test]
    fn native_bulk_syscall_returns_and_accounts_fixture_records() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("regular"), b"data").unwrap();
        let opt = Options::default();
        let mut bulk_records = 0;
        let stat = scan_with(tmp.path(), &opt, |fd, attrs, buffer| {
            let result = read_bulk_records(fd, attrs, buffer);
            if let Ok(n) = result.as_ref() {
                bulk_records += n;
            }
            result
        });
        assert!(
            bulk_records > 0,
            "native fixture must exercise GALB, not fallback"
        );
        assert_eq!((stat.files, stat.logical), (1, 4));
        assert_eq!(opt.error_count.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn initial_capability_errors_fall_back_without_scan_errors() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("regular"), b"data").unwrap();
        for errno in [libc::ENOTSUP, libc::ENOSYS, libc::EINVAL] {
            let opt = Options::default();
            let mut calls = 0;
            let stat = scan_with(tmp.path(), &opt, |_, _, _| {
                calls += 1;
                Err(std::io::Error::from_raw_os_error(errno))
            });
            assert_eq!(calls, 1);
            assert_eq!((stat.files, stat.logical), (1, 4));
            assert_eq!(opt.error_count.load(Ordering::Relaxed), 0);
        }
    }

    #[test]
    fn capability_fallback_preserves_real_metadata_errors() {
        let tmp = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(tmp.path().join("absent"), tmp.path().join("broken")).unwrap();
        let opt = Options {
            follow_links: true,
            ..Options::default()
        };
        let stat = scan_with(tmp.path(), &opt, |_, _, _| {
            Err(std::io::Error::from_raw_os_error(libc::ENOTSUP))
        });
        assert_eq!(stat.files, 0);
        assert_eq!(opt.error_count.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn non_capability_errors_do_not_restart_enumeration() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("regular"), b"data").unwrap();
        let opt = Options::default();
        let stat = scan_with(tmp.path(), &opt, |_, _, _| {
            Err(std::io::Error::from_raw_os_error(libc::EIO))
        });
        assert_eq!(stat.files, 0);
        assert_eq!(opt.error_count.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn capability_error_after_bulk_records_does_not_restart() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("regular"), b"data").unwrap();
        let opt = Options::default();
        let mut calls = 0;
        let stat = scan_with(tmp.path(), &opt, |_, _, bytes| {
            calls += 1;
            if calls > 1 {
                return Err(std::io::Error::from_raw_os_error(libc::ENOTSUP));
            }
            // One complete packed record for the existing regular file.
            bytes[..80].fill(0);
            for (offset, value) in [
                (0, 80u32),
                (4, COMMON),
                (16, FILE),
                (28, 44),
                (32, 8),
                (36, 1),
                (40, VREG),
                (52, 1),
            ] {
                bytes[offset..offset + 4].copy_from_slice(&value.to_ne_bytes());
            }
            bytes[44..52].copy_from_slice(&1u64.to_ne_bytes());
            bytes[64..72].copy_from_slice(&4i64.to_ne_bytes());
            bytes[72..80].copy_from_slice(b"regular\0");
            Ok(1)
        });
        assert_eq!(calls, 2);
        assert_eq!((stat.files, stat.logical), (1, 4));
        assert_eq!(opt.error_count.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn metadata_fallback_preserves_special_file_types() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = std::fs::File::open(tmp.path()).unwrap();
        let fifo = CString::new(tmp.path().join("fifo").as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        let _listener = std::os::unix::net::UnixListener::bind(tmp.path().join("socket")).unwrap();
        for (name, kind) in [("fifo", VFIFO), ("socket", VSOCK)] {
            let name = CString::new(name).unwrap();
            assert_eq!(
                metadata_at(dir.as_raw_fd(), &name, false).unwrap().kind,
                kind
            );
        }
        let opt = Options::default();
        let stat = scan_with(tmp.path(), &opt, |_, _, _| {
            Err(std::io::Error::from_raw_os_error(libc::ENOTSUP))
        });
        assert_eq!((stat.files, stat.logical, stat.physical), (0, 0, 0));
        assert_eq!(opt.error_count.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn request_matches_native_darwin_constants() {
        assert_eq!(
            COMMON,
            libc::ATTR_CMN_RETURNED_ATTRS
                | 0x2000_0000
                | libc::ATTR_CMN_NAME
                | libc::ATTR_CMN_DEVID
                | libc::ATTR_CMN_OBJTYPE
                | libc::ATTR_CMN_FILEID
        );
        assert_eq!(
            FILE,
            libc::ATTR_FILE_LINKCOUNT | libc::ATTR_FILE_DATALENGTH | libc::ATTR_FILE_ALLOCSIZE
        );
    }

    #[test]
    fn missing_metadata_falls_back_but_valid_zero_does_not() {
        let opt = Options::default();
        let mut entry = Entry {
            kind: Some(VREG),
            device: Some(1),
            inode: Some(2),
            links: Some(1),
            logical: Some(1024),
            physical: Some(0),
            ..Entry::default()
        };
        assert_eq!(entry.metadata(&opt).unwrap().physical, 0);
        entry.physical = Some(1);
        assert_eq!(entry.metadata(&opt).unwrap().physical, 512);
        entry.physical = None;
        assert!(entry.metadata(&opt).is_none());
        let logical_only = Options {
            compute_physical: false,
            ..opt
        };
        assert!(entry.metadata(&logical_only).is_some());
    }

    #[test]
    fn mount_and_symlink_traversal_require_target_metadata() {
        let dir = Entry {
            kind: Some(VDIR),
            device: Some(1),
            inode: Some(2),
            ..Entry::default()
        };
        assert!(dir.metadata(&Options::default()).is_some());
        assert!(dir
            .metadata(&Options {
                one_file_system: true,
                ..Options::default()
            })
            .is_none());
        assert!(dir
            .metadata(&Options {
                follow_links: true,
                ..Options::default()
            })
            .is_none());
        let link = Entry {
            kind: Some(VLNK),
            ..Entry::default()
        };
        assert!(link
            .metadata(&Options {
                follow_links: true,
                ..Options::default()
            })
            .is_none());
    }
}
