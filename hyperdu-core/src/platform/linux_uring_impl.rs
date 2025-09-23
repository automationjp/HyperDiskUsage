// Minimal io_uring STATX pipeline. Falls back to stable implementation if unavailable.
use std::{
    cell::RefCell, collections::VecDeque, ffi::CString, os::unix::ffi::OsStrExt, time::Instant,
};

use io_uring::{opcode, IoUring};

use crate::{memory_pool::BufferGuard, DirContext, ScanContext, StatMap};

struct RingCtx {
    ring: IoUring,
}

thread_local! {
    static TL_RING: RefCell<Option<RingCtx>> = const { RefCell::new(None) };
}

pub fn process_dir(ctx: &ScanContext, dctx: &DirContext, map: &mut StatMap) {
    let opt = ctx.options;
    // Try io_uring ring (once per thread)
    let mut used = false;
    let ok = TL_RING
        .try_with(|cell| {
            let mut ctx_opt = cell.borrow_mut();
            if ctx_opt.is_none() {
                // Builder with optional SQPOLL/COOP_TASKRUN flags via env (opt-in)
                let depth = opt
                    .uring_sq_depth
                    .load(std::sync::atomic::Ordering::Relaxed) as u32;
                let mut builder = IoUring::builder();
                if std::env::var("HYPERDU_URING_SQPOLL").ok().as_deref() == Some("1") {
                    let idle = std::env::var("HYPERDU_URING_SQPOLL_IDLE_MS")
                        .ok()
                        .and_then(|s| s.parse::<u32>().ok())
                        .unwrap_or(1000);
                    builder.setup_sqpoll(idle);
                    if let Some(cpu) = std::env::var("HYPERDU_URING_SQPOLL_CPU")
                        .ok()
                        .and_then(|s| s.parse::<u32>().ok())
                    {
                        builder.setup_sqpoll_cpu(cpu);
                    }
                }
                if std::env::var("HYPERDU_URING_COOP_TASKRUN").ok().as_deref() == Some("1") {
                    builder.setup_coop_taskrun();
                }
                let ring_res = builder.build(depth).or_else(|_| IoUring::new(depth));
                if let Ok(r) = ring_res {
                    *ctx_opt = Some(RingCtx { ring: r });
                }
            }
            if let Some(rctx) = ctx_opt.as_mut() {
                used = true;
                process_with_ring(&mut rctx.ring, ctx, dctx, map);
            }
        })
        .is_ok();
    if !ok || !used {
        super::linux_x86_64_impl::process_dir(ctx, dctx, map);
    }
}

#[allow(clippy::too_many_arguments)]
fn process_with_ring(ring: &mut IoUring, ctx: &ScanContext, dctx: &DirContext, map: &mut StatMap) {
    let dir = dctx.dir;
    let depth = dctx.depth;
    let resume = dctx.resume;
    let opt = ctx.options;
    // Always-inflight STATX pipeline: enumerate via getdents64, keep ring saturated
    use libc::{c_long, syscall};
    const SYS_GETDENTS64: c_long = 217;

    let c_path = match CString::new(dir.as_os_str().as_bytes()) {
        Ok(s) => s,
        Err(_) => return,
    };
    let mut open_flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC;
    if !opt.follow_links {
        open_flags |= libc::O_NOFOLLOW;
    }
    let fd = unsafe { libc::open(c_path.as_ptr(), open_flags) };
    if fd < 0 {
        crate::error_handling::record_error(
            opt,
            &crate::error_handling::last_os_error_systemcall(dir, "open"),
        );
        return;
    }
    if let Some(off) = resume {
        unsafe {
            libc::lseek(fd, off as libc::off_t, libc::SEEK_SET);
        }
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

    let stat_cur = map.entry(dir.to_path_buf()).or_default();
    let files_before = stat_cur.files;
    // getdents64 buffer via RAII thread-local pool to avoid reallocs
    fn buf_size() -> usize {
        if let Ok(s) = std::env::var("HYPERDU_GETDENTS_BUF_KB") {
            if let Ok(kb) = s.parse::<usize>() {
                return (kb.max(4)) * 1024;
            }
        }
        128 * 1024
    }
    let mut guard = BufferGuard::borrow(buf_size());
    let buf = guard.as_mut_slice();

    // Window size and slot arrays
    let sq_depth = opt
        .uring_sq_depth
        .load(std::sync::atomic::Ordering::Relaxed)
        .max(1);
    let batch_cfg = opt
        .uring_batch
        .load(std::sync::atomic::Ordering::Relaxed)
        .max(1);
    let mut window = sq_depth; // in-flight target equals SQ depth (may adapt on SQE pressure)
    let mut results: Vec<io_uring::types::statx> =
        (0..window).map(|_| unsafe { std::mem::zeroed() }).collect();
    let mut items: Vec<Option<(CString, u8)>> = (0..window).map(|_| None).collect();
    let mut free: Vec<usize> = (0..window).rev().collect(); // stack of free slot indices
    let mut inflight: usize = 0;
    let mut pending: VecDeque<(CString, u8)> = VecDeque::with_capacity(window * batch_cfg);

    let need_blocks = opt.compute_physical;
    let need_ino = !opt.count_hardlinks;
    let mut mask = libc::STATX_SIZE | libc::STATX_MODE;
    if need_blocks {
        mask |= libc::STATX_BLOCKS;
    }
    if need_ino {
        mask |= libc::STATX_INO;
    }
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

    // Metrics
    let mut enq: u64 = 0;
    let mut fail: u64 = 0;
    let mut consec_no_fail: u32 = 0;

    // Enumerate and stream submit
    let mut nread;
    loop {
        // Keep window saturated before reading more if we already have pending items
        if !pending.is_empty() {
            // Try enqueue
            {
                // Submission with retry on SQ full
                let mut sq = ring.submission();
                while inflight < window {
                    let Some((name, dt)) = pending.pop_front() else {
                        break;
                    };
                    let Some(slot) = free.pop() else { break };
                    items[slot] = Some((name, dt));
                    let (ref nm, _dt) = items[slot].as_ref().unwrap();
                    let statxbuf: *mut io_uring::types::statx = (&mut results[slot]) as *mut _;
                    let sqe = opcode::Statx::new(io_uring::types::Fd(fd), nm.as_ptr(), statxbuf)
                        .mask(mask)
                        .flags(flags)
                        .build()
                        .user_data(slot as u64);
                    if unsafe { sq.push(&sqe) }.is_ok() {
                        inflight += 1;
                        enq += 1;
                        continue;
                    }
                    drop(sq);
                    let _ = ring.submit();
                    sq = ring.submission();
                    if unsafe { sq.push(&sqe) }.is_ok() {
                        inflight += 1;
                        enq += 1;
                        continue;
                    }
                    fail += 1;
                    if window > 1 {
                        window -= 1;
                    }
                    break;
                }
                drop(sq);
            }
            if inflight == window {
                let _ = ring.submit();
            }
            // Drain completions
            {
                let mut completed = 0u64;
                for cqe in ring.completion() {
                    let res = cqe.result();
                    if res < 0 {
                        opt.uring_cqe_err
                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                    let slot = cqe.user_data() as usize;
                    if res >= 0 && slot < items.len() {
                        if let Some((ref nm, dt)) = items[slot] {
                            let stx: libc::statx = unsafe {
                                std::ptr::read_unaligned(
                                    (&results[slot]) as *const _ as *const libc::statx,
                                )
                            };
                            let mode = stx.stx_mode as u32;
                            let ftype = mode & libc::S_IFMT;
                            if ftype == libc::S_IFDIR || (ftype == 0 && dt == libc::DT_DIR) {
                                if opt.max_depth == 0 || depth < opt.max_depth {
                                    use std::ffi::OsStr;
                                    let child = dir.join(OsStr::from_bytes(nm.as_bytes()));
                                    if crate::filters::path_excluded(&child, opt) {
                                        // skip
                                    } else if opt.one_file_system {
                                        let child_dev = ((stx.stx_dev_major as u64) << 32)
                                            | (stx.stx_dev_minor as u64);
                                        if child_dev == cur_dev {
                                            ctx.normal_injector.push(crate::Job {
                                                dir: child,
                                                depth: depth + 1,
                                                resume: None,
                                            });
                                        }
                                    } else {
                                        ctx.enqueue_dir(child, depth + 1);
                                    }
                                }
                            } else if ftype == libc::S_IFREG
                                || (opt.follow_links && ftype == libc::S_IFLNK)
                                || (ftype == 0 && dt == libc::DT_REG)
                            {
                                if ftype == libc::S_IFREG {
                                    let dev = ((stx.stx_dev_major as u64) << 32)
                                        | (stx.stx_dev_minor as u64);
                                    let ino = stx.stx_ino;
                                    if crate::common_ops::check_hardlink_duplicate(opt, dev, ino) {
                                        items[slot] = None;
                                        free.push(slot);
                                        inflight -= 1;
                                        completed += 1;
                                        continue;
                                    }
                                }
                                let logical = stx.stx_size;
                                use std::ffi::OsStr;
                                let child = dir.join(OsStr::from_bytes(nm.as_bytes()));
                                if logical >= opt.min_file_size {
                                    let physical = crate::common_ops::calculate_physical_size(
                                        opt,
                                        logical,
                                        stx.stx_blocks,
                                    );
                                    crate::common_ops::update_file_stats(
                                        stat_cur, logical, physical,
                                    );
                                    crate::common_ops::report_file_progress(
                                        opt,
                                        ctx.total_files,
                                        Some(&child),
                                    );
                                } else if ftype == 0 {
                                    // immediate fallback when type info is missing
                                    if let Ok(md) = std::fs::symlink_metadata(&child) {
                                        if md.file_type().is_file() {
                                            let l = md.len();
                                            if l >= opt.min_file_size {
                                                crate::common_ops::update_file_stats(
                                                    stat_cur, l, l,
                                                );
                                                crate::common_ops::report_file_progress(
                                                    opt,
                                                    ctx.total_files,
                                                    Some(&child),
                                                );
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    } else if res < 0 && slot < items.len() {
                        // STATX failed: try metadata-based fallback
                        if let Some((ref nm, dt)) = items[slot] {
                            use std::ffi::OsStr;
                            let child = dir.join(OsStr::from_bytes(nm.as_bytes()));
                            if dt == libc::DT_DIR {
                                if (opt.max_depth == 0 || depth < opt.max_depth)
                                    && !crate::filters::path_excluded(&child, opt)
                                {
                                    ctx.enqueue_dir(child, depth + 1);
                                }
                            } else if let Ok(md) = std::fs::symlink_metadata(&child) {
                                if md.file_type().is_file() {
                                    let l = md.len();
                                    if l >= opt.min_file_size {
                                        crate::common_ops::update_file_stats(stat_cur, l, l);
                                        crate::common_ops::report_file_progress(
                                            opt,
                                            ctx.total_files,
                                            Some(&child),
                                        );
                                    }
                                }
                            }
                        }
                    }
                    items[slot] = None;
                    free.push(slot);
                    inflight -= 1;
                    completed += 1;
                }
                opt.uring_cqe_comp
                    .fetch_add(completed, std::sync::atomic::Ordering::Relaxed);
                // Adaptive grow: if no recent failures and we are saturating, cautiously increase window
                if completed > 0 && fail == 0 && inflight >= window && window < sq_depth {
                    consec_no_fail = consec_no_fail.saturating_add(1);
                    if consec_no_fail >= 3 {
                        window += 1;
                        consec_no_fail = 0;
                    }
                } else if fail > 0 {
                    consec_no_fail = 0;
                }
            }
        }

        nread = unsafe {
            syscall(
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
            let ptr = unsafe { buf.as_ptr().add(bpos as usize) };
            let reclen = unsafe { crate::platform::linux_helpers::dirent_reclen(ptr) };
            let dtype = unsafe { crate::platform::linux_helpers::dirent_dtype(ptr) };
            let name_slice =
                unsafe { crate::platform::linux_helpers::dirent_name_slice(ptr, reclen) };
            if name_slice == b"." || name_slice == b".." {
                bpos += reclen;
                continue;
            }
            if dtype == libc::DT_DIR {
                if opt.max_depth == 0 || depth < opt.max_depth {
                    let child = dir.join(std::ffi::OsStr::from_bytes(name_slice));
                    ctx.enqueue_dir(child, depth + 1);
                }
                bpos += reclen;
                continue;
            }
            if let Ok(cn) = CString::new(name_slice) {
                pending.push_back((cn, dtype));
            }
            // Try to keep ring full as we go
            {
                let mut sq = ring.submission();
                while inflight < window {
                    let Some((name, dt)) = pending.pop_front() else {
                        break;
                    };
                    let Some(slot) = free.pop() else { break };
                    items[slot] = Some((name, dt));
                    let (ref nm, _dt) = items[slot].as_ref().unwrap();
                    let statxbuf: *mut io_uring::types::statx = (&mut results[slot]) as *mut _;
                    let sqe = opcode::Statx::new(io_uring::types::Fd(fd), nm.as_ptr(), statxbuf)
                        .mask(mask)
                        .flags(flags)
                        .build()
                        .user_data(slot as u64);
                    if unsafe { sq.push(&sqe) }.is_ok() {
                        inflight += 1;
                        enq += 1;
                        continue;
                    }
                    drop(sq);
                    let _ = ring.submit();
                    sq = ring.submission();
                    if unsafe { sq.push(&sqe) }.is_ok() {
                        inflight += 1;
                        enq += 1;
                        continue;
                    }
                    fail += 1;
                    if window > 1 {
                        window -= 1;
                    }
                    break;
                }
                drop(sq);
            }
            // Drain completions opportunistically
            {
                let mut completed = 0u64;
                for cqe in ring.completion() {
                    let res = cqe.result();
                    if res < 0 {
                        opt.uring_cqe_err
                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                    let slot = cqe.user_data() as usize;
                    if res >= 0 && slot < items.len() {
                        if let Some((ref nm, _dt)) = items[slot] {
                            let stx: libc::statx = unsafe {
                                std::ptr::read_unaligned(
                                    (&results[slot]) as *const _ as *const libc::statx,
                                )
                            };
                            let mode = stx.stx_mode as u32;
                            let ftype = mode & libc::S_IFMT;
                            if ftype == libc::S_IFDIR {
                                if opt.max_depth == 0 || depth < opt.max_depth {
                                    use std::ffi::OsStr;
                                    let child = dir.join(OsStr::from_bytes(nm.as_bytes()));
                                    if opt.one_file_system {
                                        let child_dev = ((stx.stx_dev_major as u64) << 32)
                                            | (stx.stx_dev_minor as u64);
                                        if child_dev == cur_dev {
                                            ctx.normal_injector.push(crate::Job {
                                                dir: child,
                                                depth: depth + 1,
                                                resume: None,
                                            });
                                        }
                                    } else {
                                        ctx.enqueue_dir(child, depth + 1);
                                    }
                                }
                            } else if ftype == libc::S_IFREG
                                || (opt.follow_links && ftype == libc::S_IFLNK)
                            {
                                if ftype == libc::S_IFREG && !opt.count_hardlinks {
                                    if let Some(cache) = &opt.inode_cache {
                                        let dev = ((stx.stx_dev_major as u64) << 32)
                                            | (stx.stx_dev_minor as u64);
                                        let ino = stx.stx_ino;
                                        if cache.insert((dev, ino), ()).is_some() {
                                            items[slot] = None;
                                            free.push(slot);
                                            inflight -= 1;
                                            completed += 1;
                                            continue;
                                        }
                                    }
                                }
                                let logical = stx.stx_size;
                                if logical >= opt.min_file_size {
                                    let physical = if opt.compute_physical {
                                        let pr = stx.stx_blocks * 512u64;
                                        if pr == 0 {
                                            logical
                                        } else {
                                            pr
                                        }
                                    } else {
                                        logical
                                    };
                                    stat_cur.logical += logical;
                                    stat_cur.physical += physical;
                                    stat_cur.files += 1;
                                    if opt.progress_every > 0 {
                                        let n = ctx
                                            .total_files
                                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                                            + 1;
                                        if n % opt.progress_every == 0 {
                                            if let Some(cb) = &opt.progress_callback {
                                                cb(n);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    items[slot] = None;
                    free.push(slot);
                    inflight -= 1;
                    completed += 1;
                }
                opt.uring_cqe_comp
                    .fetch_add(completed, std::sync::atomic::Ordering::Relaxed);
            }
            bpos += reclen;
        }
    }
    // Final drain
    // Final drain
    let t0 = Instant::now();
    while inflight > 0 || !pending.is_empty() {
        {
            let mut sq = ring.submission();
            while inflight < window {
                let Some((name, dt)) = pending.pop_front() else {
                    break;
                };
                let Some(slot) = free.pop() else { break };
                items[slot] = Some((name, dt));
                let (ref nm, _dt) = items[slot].as_ref().unwrap();
                let statxbuf: *mut io_uring::types::statx = (&mut results[slot]) as *mut _;
                let sqe = opcode::Statx::new(io_uring::types::Fd(fd), nm.as_ptr(), statxbuf)
                    .mask(mask)
                    .flags(flags)
                    .build()
                    .user_data(slot as u64);
                if unsafe { sq.push(&sqe) }.is_ok() {
                    inflight += 1;
                    enq += 1;
                } else {
                    fail += 1;
                    break;
                }
            }
            drop(sq);
        }
        let _ = ring.submit_and_wait(1);
        {
            let mut completed = 0u64;
            for cqe in ring.completion() {
                let res = cqe.result();
                if res < 0 {
                    opt.uring_cqe_err
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                let slot = cqe.user_data() as usize;
                if res >= 0 && slot < items.len() {
                    if let Some((ref nm, dt)) = items[slot] {
                        let stx: libc::statx = unsafe {
                            std::ptr::read_unaligned(
                                (&results[slot]) as *const _ as *const libc::statx,
                            )
                        };
                        let mode = stx.stx_mode as u32;
                        let ftype = mode & libc::S_IFMT;
                        if ftype == libc::S_IFDIR || (ftype == 0 && dt == libc::DT_DIR) {
                            if opt.max_depth == 0 || depth < opt.max_depth {
                                use std::ffi::OsStr;
                                let child = dir.join(OsStr::from_bytes(nm.as_bytes()));
                                if crate::filters::path_excluded(&child, opt) {
                                    // skip
                                } else if opt.one_file_system {
                                    let child_dev = ((stx.stx_dev_major as u64) << 32)
                                        | (stx.stx_dev_minor as u64);
                                    if child_dev == cur_dev {
                                        ctx.normal_injector.push(crate::Job {
                                            dir: child,
                                            depth: depth + 1,
                                            resume: None,
                                        });
                                    }
                                } else {
                                    ctx.enqueue_dir(child, depth + 1);
                                }
                            }
                        } else if ftype == libc::S_IFREG
                            || (opt.follow_links && ftype == libc::S_IFLNK)
                            || (ftype == 0 && dt == libc::DT_REG)
                        {
                            if ftype == libc::S_IFREG {
                                let dev =
                                    ((stx.stx_dev_major as u64) << 32) | (stx.stx_dev_minor as u64);
                                let ino = stx.stx_ino;
                                if crate::common_ops::check_hardlink_duplicate(opt, dev, ino) {
                                    items[slot] = None;
                                    free.push(slot);
                                    inflight -= 1;
                                    completed += 1;
                                    continue;
                                }
                            }
                            use std::ffi::OsStr;
                            let child = dir.join(OsStr::from_bytes(nm.as_bytes()));
                            let logical = stx.stx_size;
                            if logical >= opt.min_file_size {
                                let physical = crate::common_ops::calculate_physical_size(
                                    opt,
                                    logical,
                                    stx.stx_blocks,
                                );
                                crate::common_ops::update_file_stats(stat_cur, logical, physical);
                                crate::common_ops::report_file_progress(
                                    opt,
                                    ctx.total_files,
                                    Some(&child),
                                );
                            } else if ftype == 0 {
                                if let Ok(md) = std::fs::symlink_metadata(&child) {
                                    if md.file_type().is_file() {
                                        let l = md.len();
                                        if l >= opt.min_file_size {
                                            crate::common_ops::update_file_stats(stat_cur, l, l);
                                            crate::common_ops::report_file_progress(
                                                opt,
                                                ctx.total_files,
                                                Some(&child),
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    }
                } else if res < 0 && slot < items.len() {
                    if let Some((ref nm, dt)) = items[slot] {
                        use std::ffi::OsStr;
                        let child = dir.join(OsStr::from_bytes(nm.as_bytes()));
                        if dt == libc::DT_DIR {
                            if (opt.max_depth == 0 || depth < opt.max_depth)
                                && !crate::filters::path_excluded(&child, opt)
                            {
                                ctx.enqueue_dir(child, depth + 1);
                            }
                        } else if let Ok(md) = std::fs::symlink_metadata(&child) {
                            if md.file_type().is_file() {
                                let l = md.len();
                                if l >= opt.min_file_size {
                                    crate::common_ops::update_file_stats(stat_cur, l, l);
                                    crate::common_ops::report_file_progress(
                                        opt,
                                        ctx.total_files,
                                        Some(&child),
                                    );
                                }
                            }
                        }
                    }
                }
                items[slot] = None;
                free.push(slot);
                inflight -= 1;
                completed += 1;
            }
            opt.uring_cqe_comp
                .fetch_add(completed, std::sync::atomic::Ordering::Relaxed);
            if completed > 0 && fail == 0 && inflight >= window && window < sq_depth {
                consec_no_fail = consec_no_fail.saturating_add(1);
                if consec_no_fail >= 3 {
                    window += 1;
                    consec_no_fail = 0;
                }
            } else if fail > 0 {
                consec_no_fail = 0;
            }
        }
    }
    let dt = t0.elapsed();
    opt.uring_submit_wait_ns
        .fetch_add(dt.as_nanos() as u64, std::sync::atomic::Ordering::Relaxed);
    opt.uring_sqe_enq
        .fetch_add(enq, std::sync::atomic::Ordering::Relaxed);
    opt.uring_sqe_fail
        .fetch_add(fail, std::sync::atomic::Ordering::Relaxed);

    // Fallback: If we attempted to stat non-directory entries (enq>0) but ended up
    // recognizing no files for this directory (files unchanged), re-scan this
    // directory with a conservative per-entry stat approach and metadata fallback.
    if enq > 0 && stat_cur.files == files_before {
        // Re-open directory and iterate non-directory entries only.
        let c_path = match CString::new(dir.as_os_str().as_bytes()) {
            Ok(s) => s,
            Err(_) => {
                unsafe { libc::close(fd) };
                return;
            }
        };
        let mut oflags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC;
        if !opt.follow_links {
            oflags |= libc::O_NOFOLLOW;
        }
        let fd2 = unsafe { libc::open(c_path.as_ptr(), oflags) };
        if fd2 >= 0 {
            // Buffer for getdents64
            let mut guard2 = BufferGuard::borrow(buf_size());
            let buf2 = guard2.as_mut_slice();
            loop {
                let nread2 = unsafe {
                    syscall(
                        SYS_GETDENTS64,
                        fd2,
                        buf2.as_mut_ptr() as *mut libc::c_void,
                        buf2.len(),
                    )
                } as isize;
                if nread2 <= 0 {
                    break;
                }
                let mut bpos2: isize = 0;
                while bpos2 < nread2 {
                    let ptr = unsafe { buf2.as_ptr().add(bpos2 as usize) };
                    let reclen = unsafe { crate::platform::linux_helpers::dirent_reclen(ptr) };
                    let dtype = unsafe { crate::platform::linux_helpers::dirent_dtype(ptr) };
                    let name_slice =
                        unsafe { crate::platform::linux_helpers::dirent_name_slice(ptr, reclen) };
                    bpos2 += reclen;
                    if name_slice == b"." || name_slice == b".." {
                        continue;
                    }
                    // Build full path for filtering and optional progress-path callback
                    use std::ffi::OsStr;
                    let child_path = dir.join(OsStr::from_bytes(name_slice));
                    if crate::filters::path_excluded(&child_path, opt) {
                        continue;
                    }
                    if dtype == libc::DT_DIR {
                        continue;
                    }

                    // Try statx relative to dirfd first
                    let mut logical: u64 = 0;
                    let mut physical: u64 = 0;
                    let mut ok_file = false;
                    if let Ok(cn) = CString::new(name_slice) {
                        let mut stx: libc::statx = unsafe { std::mem::zeroed() };
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
                        let mut mask = libc::STATX_SIZE | libc::STATX_MODE;
                        if opt.compute_physical {
                            mask |= libc::STATX_BLOCKS;
                        }
                        let rc = unsafe { libc::statx(fd2, cn.as_ptr(), flags, mask, &mut stx) };
                        if rc == 0 {
                            let mode = stx.stx_mode as u32;
                            let ftype = mode & libc::S_IFMT;
                            if ftype == libc::S_IFREG
                                || (opt.follow_links && ftype == libc::S_IFLNK)
                                || ftype == 0
                            {
                                logical = stx.stx_size;
                                if logical >= opt.min_file_size {
                                    physical = crate::common_ops::calculate_physical_size(
                                        opt,
                                        logical,
                                        stx.stx_blocks,
                                    );
                                    ok_file = true;
                                }
                            }
                        }
                    }
                    // Fallback to metadata if statx was unusable
                    if !ok_file {
                        if let Ok(md) = std::fs::symlink_metadata(&child_path) {
                            if md.file_type().is_file() {
                                logical = md.len();
                                if logical >= opt.min_file_size {
                                    physical = logical; // best-effort without blocks
                                    ok_file = true;
                                }
                            }
                        }
                    }
                    if ok_file {
                        crate::common_ops::update_file_stats(stat_cur, logical, physical);
                        crate::common_ops::report_file_progress(
                            opt,
                            ctx.total_files,
                            Some(&child_path),
                        );
                    }
                }
            }
            unsafe { libc::close(fd2) };
        }
    }
    unsafe { libc::close(fd) };
}
