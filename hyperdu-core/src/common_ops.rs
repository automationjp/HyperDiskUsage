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

/// Whether a file needs to go through the hardlink-dedupe map at all.
///
/// A file with one link cannot be a hardlink, so consulting the shared map for
/// it can only ever return "new" — and it still costs an insert into a map that
/// every worker shares. GNU du tracks only `st_nlink > 1` for the same reason.
/// Skipping the rest also bounds the map by the number of hardlinked files
/// rather than by the number of files scanned.
///
/// `nlink == 0` means the filesystem did not report a link count (the `statx`
/// mask came back without `STATX_NLINK`); those still go through the map, since
/// guessing would silently double-count hardlinks.
#[inline]
pub fn hardlink_candidate(opt: &Options, nlink: u32) -> bool {
    !opt.count_hardlinks && opt.inode_cache.is_some() && nlink != 1
}

/// Report progress for one processed file.
///
/// Prefer [`report_files_batch`]: one shared-counter update per directory
/// rather than one per file.
#[inline]
pub fn report_file_progress(
    opt: &Options,
    total_files: &AtomicU64,
    sample: Option<(&Path, u64, u64)>,
) {
    if opt.progress_every == 0 {
        return;
    }

    let n = total_files.fetch_add(1, Ordering::Relaxed) + 1;
    if n % opt.progress_every == 0 {
        if let Some(cb) = &opt.progress_callback {
            cb(n);
        }
        if let (Some(cb), Some((path, logical, physical))) = (&opt.progress_sample_callback, sample)
        {
            cb(&crate::ProgressSample {
                path,
                logical,
                physical,
            });
        }
    }
}

/// Batched progress: account for `n` files at once. The callbacks fire when the
/// running total crosses a multiple of `progress_every`, and `sample` is
/// evaluated only in that case, so a scan with progress disabled pays nothing
/// beyond one atomic add per directory.
///
/// The sample carries the sizes the scan already read. Handing back only a path
/// made every progress tick cost another `stat`.
#[inline]
pub fn report_files_batch(
    opt: &Options,
    total_files: &AtomicU64,
    n: u64,
    sample: impl FnOnce() -> (std::path::PathBuf, u64, u64),
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
    if let Some(cb) = &opt.progress_sample_callback {
        let (path, logical, physical) = sample();
        cb(&crate::ProgressSample {
            path: &path,
            logical,
            physical,
        });
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
        // Only the map decides. A bloom "definitely new" answer cannot, because
        // two workers that discover the same directory at once both get it: the
        // one that loses the bloom race would still have to consult the map, so
        // the map has to be the single arbiter. Insert returns the previous
        // value, which makes claiming the directory one atomic step.
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

    // Zero blocks is a real answer, not a missing one: a sparse or fully
    // punched file occupies nothing, and GNU du reports it as zero. Falling
    // back to the logical size here billed a 1 GiB sparse file as 1 GiB of
    // disk usage.
    let _ = logical;
    blocks.saturating_mul(512)
}

/// True when backends may skip building the full child path for filtering
/// (no glob/regex filters and no contains-pattern with a path separator).
#[cfg(target_os = "linux")]
#[inline]
pub fn should_fast_exclude(opt: &Options) -> bool {
    !opt.needs_path_filter
}
