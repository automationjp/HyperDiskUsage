use std::{
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
};

use crate::Options;

/// Check if a hardlink has already been counted
/// Returns true if this is a duplicate that should be skipped
#[inline]
pub fn check_hardlink_duplicate(opt: &Options, dev: u64, ino: u64) -> bool {
    if opt.count_hardlinks {
        return false;
    }

    if let Some(cache) = &opt.inode_cache {
        // DashMap returns None if key was new
        cache.insert((dev, ino), ()).is_some()
    } else {
        false
    }
}

/// Report progress for a processed file
#[inline]
pub fn report_file_progress(opt: &Options, total_files: &AtomicU64, path: Option<&Path>) {
    if opt.progress_every == 0 {
        return;
    }

    let n = total_files.fetch_add(1, Ordering::Relaxed) + 1;
    if n % opt.progress_every == 0 {
        if let Some(cb) = &opt.progress_callback {
            cb(n);
        }
        if let Some(pcb) = &opt.progress_path_callback {
            if let Some(p) = path {
                pcb(p);
            }
        }
    }
}

/// Batched progress: account for `n` files at once. The callbacks fire when the
/// running total crosses a multiple of `progress_every`; `path` is evaluated
/// only in that case.
#[inline]
pub fn report_files_batch(
    opt: &Options,
    total_files: &AtomicU64,
    n: u64,
    path: impl FnOnce() -> std::path::PathBuf,
) {
    if n == 0 || opt.progress_every == 0 {
        return;
    }
    let every = opt.progress_every;
    let prev = total_files.fetch_add(n, Ordering::Relaxed);
    let now = prev + n;
    if now / every == prev / every {
        return;
    }
    if let Some(cb) = &opt.progress_callback {
        cb(now);
    }
    if let Some(pcb) = &opt.progress_path_callback {
        pcb(&path());
    }
}

/// Check if a directory has been visited (loop detection)
/// Returns true if this directory should be skipped
#[cfg(unix)]
#[inline]
pub fn check_visited_directory(opt: &Options, dev: u64, ino: u64) -> bool {
    if !opt.follow_links {
        return false;
    }

    if let Some(vset) = &opt.visited_dirs {
        // The bloom filter only gives a fast "definitely new" answer. Either way
        // the exact set must record the entry, or the second visit would also be
        // reported as new and a cycle would be entered twice before it is cut.
        if let Some(bf) = &opt.visited_bloom {
            if !bf.test_and_set(dev, ino) {
                vset.insert((dev, ino), ());
                return false;
            }
        }

        // DashMap returns Some if key already existed
        vset.insert((dev, ino), ()).is_some()
    } else {
        false
    }
}

/// Update stats for a file
#[inline]
pub fn update_file_stats(stat_cur: &mut crate::Stat, logical: u64, physical: u64) {
    stat_cur.logical += logical;
    stat_cur.physical += physical;
    stat_cur.files += 1;
}

/// Calculate physical size from blocks (Linux-only path)
#[cfg(target_os = "linux")]
#[inline]
pub fn calculate_physical_size(opt: &Options, logical: u64, blocks: u64) -> u64 {
    if !opt.compute_physical {
        return logical;
    }

    let block_size = blocks * 512;
    if block_size == 0 {
        logical
    } else {
        block_size
    }
}

/// True when backends may skip building the full child path for filtering
/// (no glob/regex filters and no contains-pattern with a path separator).
#[cfg(target_os = "linux")]
#[inline]
pub fn should_fast_exclude(opt: &Options) -> bool {
    !opt.needs_path_filter
}
