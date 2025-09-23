use crate::Options;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

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

/// Check if a directory has been visited (loop detection)
/// Returns true if this directory should be skipped
#[inline]
pub fn check_visited_directory(opt: &Options, dev: u64, ino: u64) -> bool {
    if !opt.follow_links {
        return false;
    }

    if let Some(vset) = &opt.visited_dirs {
        // Bloom filter for fast pre-check
        if let Some(bf) = &opt.visited_bloom {
            if !bf.test_and_set(dev, ino) {
                return false; // Definitely not visited
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

/// Calculate physical size from blocks
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

/// Check if path should be excluded based on fast exclude optimization
#[inline]
pub fn should_fast_exclude(opt: &Options) -> bool {
    !opt.exclude_contains
        .iter()
        .any(|s| s.as_bytes().iter().any(|&c| c == b'/' || c == b'\\'))
}
