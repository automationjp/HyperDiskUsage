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
    // Respect follow_links: only use O_NOFOLLOW when not following links.
    let mut open_flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC;
    if !opt.follow_links {
        open_flags |= libc::O_NOFOLLOW;
    }
    let fd = unsafe { libc::open(c_path.as_ptr(), open_flags) };
    if fd < 0 {
        record_error(opt, &last_os_error_systemcall(dir, "open"));
        return;
    }
    // Current directory device id for one-file-system check
    let mut st_cur: libc::stat = unsafe { std::mem::zeroed() };
    let cur_dev: u64 = unsafe {
        if libc::fstat(fd, &mut st_cur as *mut _) == 0 {
            st_cur.st_dev
        } else {
            0
        }
    };
    // Optional prefetch hints
    #[cfg(feature = "prefetch-advise")]
    unsafe {
        if std::env::var("HYPERDU_PREFETCH").ok().as_deref() == Some("1") {
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

    fn buf_size() -> usize {
        if let Ok(s) = std::env::var("HYPERDU_GETDENTS_BUF_KB") {
            if let Ok(kb) = s.parse::<usize>() {
                return (kb.max(4)) * 1024;
            }
        }
        128 * 1024 // default: 128KB (tune NVMe/SSD friendly)
    }
    let mut guard = BufferGuard::borrow(buf_size());
    let buf = guard.as_mut_slice();
    #[cfg(feature = "prefetch-advise")]
    unsafe {
        if std::env::var("HYPERDU_PREFETCH").ok().as_deref() == Some("1") {
            let _ = libc::madvise(buf.as_mut_ptr() as *mut _, buf.len(), libc::MADV_WILLNEED);
        }
    }
    let stat_cur = map.entry(dir.to_path_buf()).or_default();
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
                    use std::ffi::OsStr;
                    // one-file-system: compare child dev to current dev
                    if opt.one_file_system {
                        #[cfg(not(target_env = "musl"))]
                        {
                            // Use statx to fetch device id once (comparable cost to fstatat)
                            let mut stx: libc::statx = unsafe { std::mem::zeroed() };
                            if let Ok(cn) = CString::new(name_slice) {
                                let mut flags = libc::AT_SYMLINK_NOFOLLOW;
                                if !matches!(
                                    opt.compat_mode,
                                    crate::CompatMode::GnuStrict | crate::CompatMode::PosixStrict
                                ) {
                                    flags |= libc::AT_STATX_DONT_SYNC;
                                    #[cfg(target_os = "linux")]
                                    {
                                        flags |= libc::AT_NO_AUTOMOUNT;
                                    }
                                }
                                let rc = unsafe {
                                    libc::statx(
                                        fd,
                                        cn.as_ptr(),
                                        flags,
                                        libc::STATX_TYPE | libc::STATX_INO | libc::STATX_MODE,
                                        &mut stx,
                                    )
                                };
                                if rc == 0 {
                                    let child_dev = ((stx.stx_dev_major as u64) << 32)
                                        | (stx.stx_dev_minor as u64);
                                    if child_dev != cur_dev {
                                        bpos += d_reclen;
                                        continue;
                                    }
                                }
                            }
                        }
                        #[cfg(target_env = "musl")]
                        {
                            // Fallback: use std metadata (best-effort)
                            use std::os::unix::fs::MetadataExt;
                            let child_path = dir.join(OsStr::from_bytes(name_slice));
                            if let Ok(md) = std::fs::symlink_metadata(&child_path) {
                                let child_dev = md.dev() as u64;
                                if child_dev != cur_dev {
                                    bpos += d_reclen;
                                    continue;
                                }
                            }
                        }
                    }
                    // Symlink loop detection (optional)
                    if opt.follow_links {
                        #[cfg(not(target_env = "musl"))]
                        {
                            if let Ok(cn) = CString::new(name_slice) {
                                let mut stx: libc::statx = unsafe { std::mem::zeroed() };
                                let mut flags = libc::AT_SYMLINK_NOFOLLOW;
                                if !matches!(
                                    opt.compat_mode,
                                    crate::CompatMode::GnuStrict | crate::CompatMode::PosixStrict
                                ) {
                                    flags |= libc::AT_STATX_DONT_SYNC;
                                    #[cfg(target_os = "linux")]
                                    {
                                        flags |= libc::AT_NO_AUTOMOUNT;
                                    }
                                }
                                let rc = unsafe {
                                    libc::statx(
                                        fd,
                                        cn.as_ptr(),
                                        flags,
                                        libc::STATX_INO | libc::STATX_MODE,
                                        &mut stx,
                                    )
                                };
                                if rc == 0 {
                                    let dev = cur_dev; // same device after -x check above
                                    let ino = stx.stx_ino;
                                    if check_visited_directory(opt, dev, ino) {
                                        bpos += d_reclen;
                                        continue;
                                    }
                                }
                            }
                        }
                        #[cfg(target_env = "musl")]
                        {
                            // Fallback: use std metadata to approximate loop detection
                            use std::os::unix::fs::MetadataExt;
                            let child_path = dir.join(OsStr::from_bytes(name_slice));
                            if let Ok(md) = std::fs::symlink_metadata(&child_path) {
                                let dev = cur_dev;
                                let ino = md.ino() as u64;
                                if check_visited_directory(opt, dev, ino) {
                                    bpos += d_reclen;
                                    continue;
                                }
                            }
                        }
                    }
                    let child_path = dir.join(OsStr::from_bytes(name_slice));
                    ctx.enqueue_dir(child_path, depth + 1);
                }
            } else if dtype == libc::DT_REG {
                // Approximate size path to avoid statx when allowed
                if !opt.compute_physical && opt.approximate_sizes && opt.min_file_size == 0 {
                    let logical = 4096u64; // estimate 4KiB per regular file
                    update_file_stats(stat_cur, logical, logical);
                    use std::ffi::OsStr;
                    let child_path = dir.join(OsStr::from_bytes(name_slice));
                    ctx.report_progress(opt, Some(&child_path));
                } else {
                    // Need precise size information
                    #[cfg(not(target_env = "musl"))]
                    {
                        let mut stx: libc::statx = unsafe { std::mem::zeroed() };
                        let c_name = match CString::new(name_slice) {
                            Ok(s) => s,
                            Err(_) => {
                                bpos += d_reclen;
                                continue;
                            }
                        };
                        let mut flags = if opt.follow_links {
                            0
                        } else {
                            libc::AT_SYMLINK_NOFOLLOW
                        };
                        if !matches!(
                            opt.compat_mode,
                            crate::CompatMode::GnuStrict | crate::CompatMode::PosixStrict
                        ) {
                            flags |= libc::AT_STATX_DONT_SYNC;
                            #[cfg(target_os = "linux")]
                            {
                                flags |= libc::AT_NO_AUTOMOUNT;
                            }
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
                            mask |= libc::STATX_INO;
                        }
                        let rc = unsafe { libc::statx(fd, c_name.as_ptr(), flags, mask, &mut stx) };
                        if rc == 0 {
                            // Hardlink dedupe (strict modes)
                            let dev =
                                ((stx.stx_dev_major as u64) << 32) | (stx.stx_dev_minor as u64);
                            let ino = stx.stx_ino;
                            if check_hardlink_duplicate(opt, dev, ino) {
                                bpos += d_reclen;
                                continue;
                            }
                            let logical = stx.stx_size;
                            if logical >= opt.min_file_size {
                                let physical =
                                    calculate_physical_size(opt, logical, stx.stx_blocks);
                                update_file_stats(stat_cur, logical, physical);
                                use std::ffi::OsStr;
                                let child_path = dir.join(OsStr::from_bytes(name_slice));
                                ctx.report_progress(opt, Some(&child_path));
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
                                    ctx.report_progress(opt, Some(&child_path));
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
                    let c_name = match CString::new(name_slice) {
                        Ok(s) => s,
                        Err(_) => {
                            bpos += d_reclen;
                            continue;
                        }
                    };
                    let mut flags = if opt.follow_links {
                        0
                    } else {
                        libc::AT_SYMLINK_NOFOLLOW
                    };
                    if !matches!(
                        opt.compat_mode,
                        crate::CompatMode::GnuStrict | crate::CompatMode::PosixStrict
                    ) {
                        flags |= libc::AT_STATX_DONT_SYNC;
                        #[cfg(target_os = "linux")]
                        {
                            flags |= libc::AT_NO_AUTOMOUNT;
                        }
                    }
                    #[cfg(any(feature = "prof-tracy", feature = "prof-puffin"))]
                    profiling::scope!("statx_unknown");
                    let need_blocks = opt.compute_physical;
                    let need_ino = !opt.count_hardlinks;
                    let mut mask = libc::STATX_SIZE | libc::STATX_MODE; // MODE needed to detect type in unknown branch
                    if need_blocks {
                        mask |= libc::STATX_BLOCKS;
                    }
                    if need_ino {
                        mask |= libc::STATX_INO;
                    }
                    let rc = unsafe { libc::statx(fd, c_name.as_ptr(), flags, mask, &mut stx) };
                    if rc == 0 {
                        let mode = stx.stx_mode as u32;
                        let ftype = mode & libc::S_IFMT;
                        if ftype == libc::S_IFDIR {
                            if opt.max_depth == 0 || depth < opt.max_depth {
                                use std::ffi::OsStr;
                                let child_path = dir.join(OsStr::from_bytes(name_slice));
                                ctx.normal_injector.push(crate::Job {
                                    dir: child_path,
                                    depth: depth + 1,
                                    resume: None,
                                });
                            }
                        } else if ftype == libc::S_IFREG
                            || (opt.follow_links && ftype == libc::S_IFLNK)
                        {
                            // Dedupe only for regular files
                            if ftype == libc::S_IFREG
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
                                use std::ffi::OsStr;
                                let child_path = dir.join(OsStr::from_bytes(name_slice));
                                ctx.report_progress(opt, Some(&child_path));
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
                                    ctx.report_progress(opt, Some(&child_path));
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
                                ctx.report_progress(opt, Some(&child_path));
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
                ctx.enqueue_resume(dir.to_path_buf(), depth, d_off);
                unsafe { libc::close(fd) };
                return;
            }
        }
    }
    unsafe { libc::close(fd) };
}
