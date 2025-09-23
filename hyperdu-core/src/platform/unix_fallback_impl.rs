use std::sync::atomic::Ordering;

use crate::{
    common_ops::{
        calculate_physical_size, check_hardlink_duplicate, check_visited_directory,
        report_file_progress, update_file_stats,
    },
    error_handling::{last_os_error_systemcall, record_error},
    name_contains_patterns_bytes, DirContext, ScanContext, StatMap,
};

pub fn process_dir(ctx: &ScanContext, dctx: &DirContext, map: &mut StatMap) {
    let dir = dctx.dir;
    let depth = dctx.depth;
    let resume = dctx.resume;
    let opt = ctx.options;
    use std::{
        ffi::{CString, OsStr},
        os::unix::ffi::OsStrExt,
    };

    let c_path = CString::new(dir.as_os_str().as_bytes()).ok();
    let Some(c_path) = c_path else { return };
    let fd = unsafe {
        libc::open(
            c_path.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        ctx.total_files.fetch_add(0, Ordering::Relaxed); // keep Ordering imported
        record_error(opt, &last_os_error_systemcall(dir, "open"));
        return;
    }
    let d = unsafe { libc::fdopendir(fd) };
    if d.is_null() {
        unsafe { libc::close(fd) };
        return;
    }
    let dirfd = unsafe { libc::dirfd(d) };
    // Current dir dev
    let mut st_cur: libc::stat = unsafe { std::mem::zeroed() };
    let cur_dev: u64 = unsafe {
        if libc::fstat(dirfd, &mut st_cur as *mut _) == 0 {
            st_cur.st_dev as u64
        } else {
            0
        }
    };
    if let Some(cookie) = resume {
        unsafe { libc::seekdir(d, cookie as libc::c_long) }
    }

    // Fast-path: if exclude patterns contain no path separators, we can
    // skip per-file full path construction and rely on name-bytes matching.
    let fast_exclude = !opt
        .exclude_contains
        .iter()
        .any(|s| s.as_bytes().iter().any(|&c| c == b'/' || c == b'\\'));

    // Pre-fetch the stats entry for current directory to avoid repeated lookups
    let stat_cur = map.entry(dir.to_path_buf()).or_default();

    let mut yield_every = opt.dir_yield_every.load(Ordering::Relaxed);
    let mut processed: usize = 0;
    loop {
        #[cfg(any(feature = "prof-tracy", feature = "prof-puffin"))]
        profiling::scope!("readdir_loop");
        let ent = unsafe { libc::readdir(d) };
        if ent.is_null() {
            break;
        }
        let entry = unsafe { &*ent };
        if entry.d_name[0] == 0 {
            continue;
        }
        let name_c = unsafe { std::ffi::CStr::from_ptr(entry.d_name.as_ptr()) };
        let name_b = name_c.to_bytes();
        if name_b == b"." || name_b == b".." {
            continue;
        }
        if crate::name_matches(name_b, opt) {
            continue;
        }
        if !fast_exclude {
            let child = dir.join(OsStr::from_bytes(name_b));
            if crate::path_excluded(&child, opt) {
                continue;
            }
        }

        let dtype = entry.d_type;
        let is_dir = dtype == libc::DT_DIR;
        let is_lnk = dtype == libc::DT_LNK;
        if is_lnk && !opt.follow_links {
            continue;
        }

        if is_dir {
            if opt.max_depth == 0 || depth < opt.max_depth {
                let child = dir.join(OsStr::from_bytes(name_b));
                if opt.one_file_system {
                    let mut st_child: libc::stat = unsafe { std::mem::zeroed() };
                    let cn = CString::new(name_b).ok();
                    if let Some(cn) = cn {
                        let rc = unsafe {
                            libc::fstatat(
                                dirfd,
                                cn.as_ptr(),
                                &mut st_child,
                                libc::AT_SYMLINK_NOFOLLOW,
                            )
                        };
                        if rc == 0 && (st_child.st_dev as u64) != cur_dev {
                            continue;
                        }
                    }
                }
                if opt.follow_links {
                    if let Some(vset) = &opt.visited_dirs {
                        let mut st: libc::stat = unsafe { std::mem::zeroed() };
                        let cn = CString::new(name_b).ok();
                        if let Some(cn) = cn {
                            let rc = unsafe {
                                libc::fstatat(
                                    dirfd,
                                    cn.as_ptr(),
                                    &mut st,
                                    libc::AT_SYMLINK_NOFOLLOW,
                                )
                            };
                            if rc == 0 {
                                let dev = st.st_dev as u64;
                                let ino = st.st_ino as u64;
                                if let Some(bf) = &opt.visited_bloom {
                                    if bf.test_and_set(dev, ino) {
                                        continue;
                                    }
                                }
                                if vset.insert((dev, ino), ()).is_some() {
                                    continue;
                                }
                            }
                        }
                    }
                }
                ctx.enqueue_dir(child, depth + 1);
            }
        } else if dtype == libc::DT_REG {
            if !opt.compute_physical && opt.approximate_sizes && opt.min_file_size == 0 {
                let logical = 4096u64;
                stat_cur.logical += logical;
                stat_cur.physical += logical;
                stat_cur.files += 1;
                if opt.progress_every > 0 {
                    let n = ctx.total_files.fetch_add(1, Ordering::Relaxed) + 1;
                    if n % opt.progress_every == 0 {
                        if let Some(cb) = &opt.progress_callback {
                            cb(n);
                        }
                        if let Some(pcb) = &opt.progress_path_callback {
                            let child = dir.join(OsStr::from_bytes(name_b));
                            pcb(&child);
                        }
                    }
                }
                processed += 1;
                if processed % 4096 == 0 {
                    yield_every = opt.dir_yield_every.load(Ordering::Relaxed);
                }
                if yield_every > 0 && processed % yield_every == 0 {
                    let cookie = unsafe { libc::telldir(d) } as u64;
                    ctx.enqueue_resume(dir.to_path_buf(), depth, cookie);
                    unsafe { libc::closedir(d) };
                    return;
                }
                continue;
            }
            // else fall through to statx path for size information
        } else {
            let mut stx: libc::statx = unsafe { std::mem::zeroed() };
            let c_name = CString::new(name_b).ok();
            if let Some(c_name) = c_name {
                let flags = if opt.follow_links {
                    0
                } else {
                    libc::AT_SYMLINK_NOFOLLOW
                };
                let rc = unsafe {
                    libc::statx(
                        dirfd,
                        c_name.as_ptr(),
                        flags,
                        libc::STATX_SIZE | libc::STATX_BLOCKS | libc::STATX_INO | libc::STATX_MODE,
                        &mut stx,
                    )
                };
                if rc == 0 {
                    // Dedupe for regular files only
                    let logical = stx.stx_size;
                    if logical >= opt.min_file_size {
                        if !opt.count_hardlinks {
                            let dev =
                                ((stx.stx_dev_major as u64) << 32) | (stx.stx_dev_minor as u64);
                            let ino = stx.stx_ino;
                            if check_hardlink_duplicate(opt, dev, ino) {
                                continue;
                            }
                        }
                        let physical_blocks = stx.stx_blocks * 512u64;
                        let physical_eff = if opt.compute_physical {
                            if physical_blocks == 0 {
                                logical
                            } else {
                                physical_blocks
                            }
                        } else {
                            logical
                        };
                        stat_cur.logical += logical;
                        stat_cur.physical += physical_eff;
                        stat_cur.files += 1;
                        if opt.progress_every > 0 {
                            let n = ctx.total_files.fetch_add(1, Ordering::Relaxed) + 1;
                            if n % opt.progress_every == 0 {
                                if let Some(cb) = &opt.progress_callback {
                                    cb(n);
                                }
                                if let Some(pcb) = &opt.progress_path_callback {
                                    let child = dir.join(OsStr::from_bytes(name_b));
                                    pcb(&child);
                                }
                            }
                        }
                    }
                } else {
                    let child = dir.join(OsStr::from_bytes(name_b));
                    if let Ok(md) = std::fs::symlink_metadata(&child) {
                        if md.file_type().is_file() {
                            let logical = md.len();
                            if logical >= opt.min_file_size {
                                stat_cur.logical += logical;
                                stat_cur.physical += logical;
                                stat_cur.files += 1;
                                if opt.progress_every > 0 {
                                    let n = ctx.total_files.fetch_add(1, Ordering::Relaxed) + 1;
                                    if n % opt.progress_every == 0 {
                                        if let Some(cb) = &opt.progress_callback {
                                            cb(n);
                                        }
                                        if let Some(pcb) = &opt.progress_path_callback {
                                            pcb(&child);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        processed += 1;
        if processed % 4096 == 0 {
            yield_every = opt.dir_yield_every.load(Ordering::Relaxed);
        }
        if yield_every > 0 && processed % yield_every == 0 {
            let cookie = unsafe { libc::telldir(d) } as u64;
            ctx.enqueue_resume(dir.to_path_buf(), depth, cookie);
            unsafe { libc::closedir(d) };
            return;
        }
    }
    unsafe { libc::closedir(d) };
}
