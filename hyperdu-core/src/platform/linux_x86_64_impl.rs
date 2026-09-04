use std::sync::atomic::Ordering;

use crate::{
    common_ops::{
        calculate_physical_size, check_hardlink_duplicate, check_visited_directory,
        should_fast_exclude, update_file_stats,
    },
    error_handling::{last_os_error_systemcall, record_error},
    memory_pool::BufferGuard,
    name_matches, DirContext, ScanContext, StatMap,
};

pub fn process_dir(ctx: &ScanContext, dctx: &DirContext, map: &mut StatMap) {
    let dir = dctx.dir;
    let depth = dctx.depth;
    let resume = dctx.resume;
    let opt = ctx.options;
    use std::{ffi::CString, os::unix::ffi::OsStrExt};

    const SYS_GETDENTS64: libc::c_long = 217; // x86_64
                                              // Fast-path: if exclude patterns contain no path separators, we can
                                              // skip per-file full path construction and rely on name-bytes matching.
    let fast_exclude = should_fast_exclude(opt);
    let c_path = match CString::new(dir.as_os_str().as_bytes()) {
        Ok(s) => s,
        Err(_) => return,
    };
    let fd = crate::platform::linux_helpers::open_dir_readonly(&c_path, opt.follow_links);
    if fd < 0 {
        record_error(opt, &last_os_error_systemcall(dir, "open"));
        return;
    }
    // Decide about this directory from its own descriptor rather than from a
    // `statx` the parent issued on its behalf. One `fstat` answers both
    // questions, the identity is the real one even when we arrived through a
    // symlink, and the scan root is covered like any other directory. When
    // neither question is asked, the syscall is skipped entirely.
    if opt.one_file_system || crate::follows_links(opt) {
        let mut st_cur: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstat(fd, &mut st_cur as *mut _) } == 0 {
            let dev = crate::platform::linux_helpers::packed_dev(st_cur.st_dev);
            // root_fs_id is zero only when the root itself could not be stat'd,
            // in which case there is no boundary to enforce.
            let leaves_root_fs =
                opt.one_file_system && opt.root_fs_id != 0 && dev != opt.root_fs_id;
            if leaves_root_fs || check_visited_directory(opt, dev, st_cur.st_ino) {
                unsafe { libc::close(fd) };
                return;
            }
        } else if opt.one_file_system {
            // Cannot confirm which filesystem this is, so `-x` must not cross.
            record_error(opt, &last_os_error_systemcall(dir, "fstat"));
            unsafe { libc::close(fd) };
            return;
        }
    }
    // Optional prefetch hints
    #[cfg(feature = "prefetch-advise")]
    unsafe {
        if opt.io_prefetch {
            let _ = libc::posix_fadvise(fd, 0, 0, libc::POSIX_FADV_SEQUENTIAL);
            let ra: libc::size_t = 1 << 20; // 1MiB
            let _ = libc::readahead(fd, 0, ra);
        }
    }
    if let Some(off) = resume {
        unsafe {
            libc::lseek(fd, off as libc::off_t, libc::SEEK_SET);
        }
    }

    let mut guard = BufferGuard::borrow(opt.getdents_buf_bytes);
    let buf = guard.as_mut_slice();
    let stat_cur = map.entry(dir.to_path_buf()).or_default();
    // Progress is accounted once per directory. Touching the shared counter and
    // building a PathBuf for every file costs more than the scan itself once the
    // sizes come from a cheap `statx`.
    let mut counted = crate::platform::linux_helpers::FileCounter::new(opt);
    let mut yield_every = opt.dir_yield_every.load(Ordering::Relaxed);
    let mut processed: usize = 0;
    loop {
        #[cfg(any(feature = "prof-tracy", feature = "prof-puffin"))]
        profiling::scope!("getdents64_loop");
        let nread = unsafe {
            libc::syscall(
                SYS_GETDENTS64,
                fd,
                buf.as_mut_ptr() as *mut libc::c_void,
                buf.len(),
            )
        } as isize;
        if nread <= 0 {
            break;
        }
        let mut bpos: isize = 0;
        while bpos < nread {
            let ptr = unsafe { buf.as_ptr().offset(bpos) };
            // Prefetch next dirent to L1 (optional)
            #[cfg(all(target_arch = "x86_64", feature = "simd-prefetch"))]
            unsafe {
                use core::arch::x86_64::_mm_prefetch;
                const _MM_HINT_T0: i32 = 3;
                let next = ptr.add(crate::platform::linux_helpers::dirent_reclen(ptr) as usize);
                _mm_prefetch(next as *const i8, _MM_HINT_T0);
            }
            let d_off = unsafe { crate::platform::linux_helpers::dirent_d_off(ptr) };
            let d_reclen = unsafe { crate::platform::linux_helpers::dirent_reclen(ptr) };
            let d_type = unsafe { crate::platform::linux_helpers::dirent_dtype(ptr) };
            let name_slice =
                unsafe { crate::platform::linux_helpers::dirent_name_slice(ptr, d_reclen) };
            if name_slice == b"." || name_slice == b".." {
                bpos += d_reclen;
                continue;
            }
            if name_matches(name_slice, opt) {
                bpos += d_reclen;
                continue;
            }

            let dtype = d_type;
            let is_dir_hint = dtype == libc::DT_DIR;
            let is_lnk = dtype == libc::DT_LNK;

            if !fast_exclude {
                use std::ffi::OsStr;
                let child_path = dir.join(OsStr::from_bytes(name_slice));
                if crate::path_excluded(&child_path, opt) {
                    bpos += d_reclen;
                    continue;
                }
            }
            if is_lnk && !opt.follow_links {
                bpos += d_reclen;
                continue;
            }

            if is_dir_hint {
                if opt.max_depth == 0 || depth < opt.max_depth {
                    // No per-child `statx` here. The filesystem-boundary and
                    // cycle checks happen when the child is opened, which is
                    // both cheaper (one `fstat` on a descriptor we need anyway)
                    // and correct for symlinked directories, whose identity the
                    // parent cannot see.
                    use std::ffi::OsStr;
                    let child_path = dir.join(OsStr::from_bytes(name_slice));
                    ctx.enqueue_dir(child_path, depth + 1);
                }
            } else if dtype == libc::DT_REG {
                // Approximate size path to avoid statx when allowed
                if !opt.compute_physical && opt.approximate_sizes && opt.min_file_size == 0 {
                    let logical = 4096u64; // estimate 4KiB per regular file
                    update_file_stats(stat_cur, logical, logical);
                    counted.record(name_slice, logical, logical);
                } else {
                    // Need precise size information
                    #[cfg(not(target_env = "musl"))]
                    {
                        let mut stx: libc::statx = unsafe { std::mem::zeroed() };
                        let name_ptr =
                            unsafe { crate::platform::linux_helpers::dirent_name_ptr(ptr) };
                        let mut flags = if opt.follow_links {
                            0
                        } else {
                            libc::AT_SYMLINK_NOFOLLOW
                        };
                        flags |= libc::AT_NO_AUTOMOUNT;
                        if !matches!(
                            opt.compat_mode,
                            crate::CompatMode::GnuStrict | crate::CompatMode::PosixStrict
                        ) {
                            flags |= libc::AT_STATX_DONT_SYNC;
                        }
                        #[cfg(any(feature = "prof-tracy", feature = "prof-puffin"))]
                        profiling::scope!("statx_reg");
                        let need_blocks = opt.compute_physical;
                        let need_ino = !opt.count_hardlinks;
                        // For REG/LNK we don't need MODE; shrink mask
                        let mut mask = libc::STATX_SIZE;
                        if need_blocks {
                            mask |= libc::STATX_BLOCKS;
                        }
                        if need_ino {
                            // NLINK comes from the same inode, so asking for it
                            // costs nothing and lets most files skip the map.
                            mask |= libc::STATX_INO | libc::STATX_NLINK;
                        }
                        let rc = unsafe { libc::statx(fd, name_ptr, flags, mask, &mut stx) };
                        if rc == 0 {
                            // Hardlink dedupe: only files that can actually be
                            // hardlinks need the shared map.
                            if crate::common_ops::hardlink_candidate(opt, stx.stx_nlink) {
                                let dev =
                                    ((stx.stx_dev_major as u64) << 32) | (stx.stx_dev_minor as u64);
                                if check_hardlink_duplicate(opt, dev, stx.stx_ino) {
                                    bpos += d_reclen;
                                    continue;
                                }
                            }
                            let logical = stx.stx_size;
                            if logical >= opt.min_file_size {
                                let physical =
                                    calculate_physical_size(opt, logical, stx.stx_blocks);
                                update_file_stats(stat_cur, logical, physical);
                                counted.record(name_slice, logical, physical);
                            }
                        }
                    }
                    #[cfg(target_env = "musl")]
                    {
                        use std::ffi::OsStr;
                        let child_path = dir.join(OsStr::from_bytes(name_slice));
                        if let Ok(md) = std::fs::symlink_metadata(&child_path) {
                            if md.file_type().is_file() {
                                let logical = md.len();
                                if logical >= opt.min_file_size {
                                    let physical = logical; // best effort on musl
                                    update_file_stats(stat_cur, logical, physical);
                                    counted.record(name_slice, logical, physical);
                                }
                            }
                        }
                    }
                }
            } else {
                // Unknown type or special file - need full stat information
                #[cfg(not(target_env = "musl"))]
                {
                    let mut stx: libc::statx = unsafe { std::mem::zeroed() };
                    let name_ptr = unsafe { crate::platform::linux_helpers::dirent_name_ptr(ptr) };
                    let mut flags = if opt.follow_links {
                        0
                    } else {
                        libc::AT_SYMLINK_NOFOLLOW
                    };
                    flags |= libc::AT_NO_AUTOMOUNT;
                    if !matches!(
                        opt.compat_mode,
                        crate::CompatMode::GnuStrict | crate::CompatMode::PosixStrict
                    ) {
                        flags |= libc::AT_STATX_DONT_SYNC;
                    }
                    #[cfg(any(feature = "prof-tracy", feature = "prof-puffin"))]
                    profiling::scope!("statx_unknown");
                    let need_blocks = opt.compute_physical;
                    // The inode is also what cycle detection keys on, so it is
                    // required whenever links are followed.
                    let need_ino = !opt.count_hardlinks || opt.follow_links;
                    let mut mask = libc::STATX_SIZE | libc::STATX_MODE; // MODE needed to detect type in unknown branch
                    if need_blocks {
                        mask |= libc::STATX_BLOCKS;
                    }
                    if need_ino {
                        mask |= libc::STATX_INO | libc::STATX_NLINK;
                    }
                    let rc = unsafe { libc::statx(fd, name_ptr, flags, mask, &mut stx) };
                    if rc == 0 {
                        let mode = stx.stx_mode as u32;
                        let ftype = mode & libc::S_IFMT;
                        if ftype == libc::S_IFDIR {
                            if opt.max_depth == 0 || depth < opt.max_depth {
                                use std::ffi::OsStr;
                                // Boundary and cycle checks happen when this
                                // directory is opened, as in the DT_DIR branch.
                                let child_path = dir.join(OsStr::from_bytes(name_slice));
                                ctx.enqueue_dir(child_path, depth + 1);
                            }
                        } else if ftype == libc::S_IFREG
                            || (opt.follow_links && ftype == libc::S_IFLNK)
                        {
                            // Dedupe only for regular files that can actually
                            // be hardlinks.
                            if ftype == libc::S_IFREG
                                && crate::common_ops::hardlink_candidate(opt, stx.stx_nlink)
                                && check_hardlink_duplicate(
                                    opt,
                                    ((stx.stx_dev_major as u64) << 32) | (stx.stx_dev_minor as u64),
                                    stx.stx_ino,
                                )
                            {
                                bpos += d_reclen;
                                continue;
                            }
                            let logical = stx.stx_size;
                            if logical >= opt.min_file_size {
                                let physical =
                                    calculate_physical_size(opt, logical, stx.stx_blocks);
                                update_file_stats(stat_cur, logical, physical);
                                counted.record(name_slice, logical, physical);
                            }
                        }
                    } else {
                        use std::ffi::OsStr;
                        let child_path = dir.join(OsStr::from_bytes(name_slice));
                        if let Ok(md) = std::fs::symlink_metadata(&child_path) {
                            if md.file_type().is_dir() {
                                if opt.max_depth == 0 || depth < opt.max_depth {
                                    ctx.enqueue_dir(child_path, depth + 1);
                                }
                            } else if md.file_type().is_file() {
                                let logical = md.len();
                                if logical >= opt.min_file_size {
                                    update_file_stats(stat_cur, logical, logical);
                                    counted.record(name_slice, logical, logical);
                                }
                            }
                        }
                    }
                }
                #[cfg(target_env = "musl")]
                {
                    use std::ffi::OsStr;
                    let child_path = dir.join(OsStr::from_bytes(name_slice));
                    if let Ok(md) = std::fs::symlink_metadata(&child_path) {
                        if md.file_type().is_dir() {
                            if opt.max_depth == 0 || depth < opt.max_depth {
                                ctx.enqueue_dir(child_path, depth + 1);
                            }
                        } else if md.file_type().is_file() {
                            let logical = md.len();
                            if logical >= opt.min_file_size {
                                update_file_stats(stat_cur, logical, logical);
                                counted.record(name_slice, logical, logical);
                            }
                        }
                    }
                }
            }

            bpos += d_reclen;
            processed += 1;
            // Refresh occasionally in case of live tuning
            if processed % 4096 == 0 {
                yield_every = opt.dir_yield_every.load(Ordering::Relaxed);
            }
            if yield_every > 0 && processed % yield_every == 0 {
                // Enqueue continuation from current offset and stop to let other threads proceed
                counted.flush(ctx, opt, dir);
                ctx.enqueue_resume(dir.to_path_buf(), depth, d_off);
                unsafe { libc::close(fd) };
                return;
            }
        }
    }
    counted.flush(ctx, opt, dir);
    unsafe { libc::close(fd) };
}
