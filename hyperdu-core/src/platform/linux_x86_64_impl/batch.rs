//! Spreading the `statx` calls of one huge directory over every worker.
//!
//! A directory is read by one thread, and with the sizes coming from a
//! per-file `statx` that thread is the whole scan when the directory is flat:
//! 200,000 files took 346 ms at one thread and 360 ms at sixteen. Once a
//! directory has shown itself to be large, its remaining regular files are cut
//! into batches that any idle worker takes. Small directories never get here.
use std::{ffi::CStr, sync::Arc};

use crate::{
    common_ops::{calculate_physical_size, check_hardlink_duplicate, update_file_stats},
    error_handling::record_error,
    scheduler::{BatchFd, StatBatch},
    DirContext, ScanContext, Stat, StatMap,
};

/// Entries read from a directory before it counts as large.
pub(super) const LARGE_AFTER: usize = 4096;
/// Files per batch: about a millisecond of `statx`, long enough to amortize the
/// hand-off and short enough to spread over every worker.
const BATCH_FILES: usize = 1024;

/// Names collected for the next batch of one directory.
pub(super) struct Pending {
    names: Vec<u8>,
    count: usize,
    fd: Option<Arc<BatchFd>>,
}

impl Pending {
    pub fn new() -> Self {
        Self {
            names: Vec::new(),
            count: 0,
            fd: None,
        }
    }

    /// Queue `name`; cuts a batch when it is full.
    pub fn push(
        &mut self,
        ctx: &ScanContext,
        dctx: &DirContext,
        fd: libc::c_int,
        stat_cur: &mut Stat,
        name: &[u8],
    ) {
        self.names.extend_from_slice(name);
        self.names.push(0);
        self.count += 1;
        if self.count >= BATCH_FILES {
            self.flush(ctx, dctx, fd, stat_cur);
        }
    }

    pub fn flush(
        &mut self,
        ctx: &ScanContext,
        dctx: &DirContext,
        fd: libc::c_int,
        stat_cur: &mut Stat,
    ) {
        if self.count == 0 {
            return;
        }
        let shared = match &self.fd {
            Some(shared) => shared.clone(),
            None => {
                // SAFETY: `fd` is the caller's open directory descriptor.
                let dup = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) };
                if dup < 0 {
                    // Out of descriptors: the batch cannot leave this thread, but
                    // the caller's descriptor is still open, so stat it in place.
                    let batch = self.take(Arc::new(BatchFd(-1)));
                    run(ctx, dctx, fd, &batch, stat_cur);
                    return;
                }
                let shared = Arc::new(BatchFd(dup));
                self.fd = Some(shared.clone());
                shared
            }
        };
        let batch = self.take(shared);
        ctx.enqueue_stat_batch(dctx.dir.to_path_buf(), dctx.depth, batch);
    }

    fn take(&mut self, fd: Arc<BatchFd>) -> StatBatch {
        let count = std::mem::take(&mut self.count);
        StatBatch {
            fd,
            names: std::mem::take(&mut self.names),
            count,
        }
    }
}

/// Run a batch another worker took from the queue.
pub(super) fn process_batch(
    ctx: &ScanContext,
    dctx: &DirContext,
    batch: &StatBatch,
    map: &mut StatMap,
) {
    let stat_cur = map.entry(dctx.dir.to_path_buf()).or_default();
    run(ctx, dctx, batch.fd.0, batch, stat_cur);
}

fn run(
    ctx: &ScanContext,
    dctx: &DirContext,
    fd: libc::c_int,
    batch: &StatBatch,
    stat_cur: &mut Stat,
) {
    let opt = ctx.options;
    let dir = dctx.dir;
    let strict_accounting = matches!(
        opt.compat_mode,
        crate::CompatMode::GnuStrict | crate::CompatMode::PosixStrict
    );
    let mut counted = crate::platform::linux_helpers::FileCounter::new(opt);
    for raw in batch.names.split_inclusive(|&b| b == 0).take(batch.count) {
        let Ok(name) = CStr::from_bytes_with_nul(raw) else {
            continue;
        };
        let name_bytes = name.to_bytes();
        if strict_accounting {
            match super::strict::metadata(fd, name, opt.follow_links) {
                Ok(m) if m.is_dir() => {}
                Ok(m) => {
                    let duplicate = crate::common_ops::hardlink_candidate(opt, m.nlink)
                        && check_hardlink_duplicate(opt, m.dev, m.ino);
                    if !duplicate && m.logical >= opt.min_file_size {
                        let physical = calculate_physical_size(opt, m.logical, m.blocks);
                        update_file_stats(stat_cur, m.logical, physical);
                        counted.record(name_bytes, m.logical, physical);
                    }
                }
                Err(source) => {
                    use std::{ffi::OsStr, os::unix::ffi::OsStrExt};
                    record_error(
                        opt,
                        &crate::error_handling::ScanError::IoError {
                            path: dir.join(OsStr::from_bytes(name_bytes)),
                            source,
                        },
                    );
                }
            }
            continue;
        }
        // Same request as the inline path for a regular file.
        // SAFETY: libc::statx is an integer-only output structure; zero is valid.
        let mut stx: libc::statx = unsafe { std::mem::zeroed() };
        let flags = (if opt.follow_links {
            0
        } else {
            libc::AT_SYMLINK_NOFOLLOW
        }) | libc::AT_NO_AUTOMOUNT
            | libc::AT_STATX_DONT_SYNC;
        let mut mask = libc::STATX_SIZE;
        if opt.compute_physical {
            mask |= libc::STATX_BLOCKS;
        }
        if !opt.count_hardlinks {
            mask |= libc::STATX_INO | libc::STATX_NLINK;
        }
        // SAFETY: `name` is NUL-terminated and `stx` is writable.
        if unsafe { libc::statx(fd, name.as_ptr(), flags, mask, &mut stx) } != 0 {
            continue;
        }
        if crate::common_ops::hardlink_candidate(opt, stx.stx_nlink) {
            let dev = ((stx.stx_dev_major as u64) << 32) | (stx.stx_dev_minor as u64);
            if check_hardlink_duplicate(opt, dev, stx.stx_ino) {
                continue;
            }
        }
        let logical = stx.stx_size;
        if logical >= opt.min_file_size {
            let physical = calculate_physical_size(opt, logical, stx.stx_blocks);
            update_file_stats(stat_cur, logical, physical);
            counted.record(name_bytes, logical, physical);
        }
    }
    counted.flush(ctx, opt, dir);
}
