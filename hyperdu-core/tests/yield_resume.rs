//! `dir_yield_every` splits a large directory into resume jobs. Those jobs
//! re-enter the same directory, which used to collide with the follow-links
//! bookkeeping and silently drop everything past the first chunk.

use std::{fs, sync::atomic::Ordering};

use hyperdu_core::{scan_directory, Options};

/// Enough entries that the yield thresholds below split the directory several
/// times. Kept small enough to stay fast on CI.
const ENTRIES: usize = 300;

fn dir_with_files(n: usize) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for i in 0..n {
        fs::File::create(dir.path().join(format!("f{i:05}"))).unwrap();
    }
    dir
}

fn count_files(root: &std::path::Path, follow_links: bool, yield_every: usize) -> u64 {
    let opt = Options {
        follow_links,
        compute_physical: false,
        ..Default::default()
    };
    opt.dir_yield_every.store(yield_every, Ordering::Relaxed);
    let map = scan_directory(root, &opt).unwrap();
    map.get(root).map(|s| s.files).unwrap_or(0)
}

/// Splitting must not change the answer. The visited-directory set claims a
/// directory as it tests it, so a resume job that re-ran the test found the
/// claim its own first pass had made, concluded it had already been here, and
/// returned -- losing every entry after the first chunk. `--follow-links
/// --dir-yield-every 5000` on a 20k-file directory reported 5000 files.
#[test]
fn yield_split_counts_every_file_with_follow_links() {
    let dir = dir_with_files(ENTRIES);
    let root = dir.path();

    let baseline = count_files(root, false, 0);
    assert_eq!(baseline as usize, ENTRIES, "baseline must see every file");

    for yield_every in [0, 32, 100, ENTRIES - 1, ENTRIES, ENTRIES * 2] {
        let got = count_files(root, true, yield_every);
        assert_eq!(
            got as usize, ENTRIES,
            "follow_links with dir_yield_every={yield_every} saw {got} of {ENTRIES} files"
        );
    }
}

/// The same split without follow-links never regressed, but it is the control:
/// if this breaks, the cause is the splitting itself rather than the
/// interaction under test.
#[test]
fn yield_split_counts_every_file_without_follow_links() {
    let dir = dir_with_files(ENTRIES);
    let root = dir.path();

    for yield_every in [0, 32, 100, ENTRIES * 2] {
        let got = count_files(root, false, yield_every);
        assert_eq!(
            got as usize, ENTRIES,
            "dir_yield_every={yield_every} saw {got} of {ENTRIES} files"
        );
    }
}

/// A real revisit still has to be caught: following a symlink into a directory
/// already scanned must count it once, which is what the visited set is for.
/// Guards against "fixing" the resume case by disabling the check outright.
#[cfg(unix)]
#[test]
fn follow_links_still_dedupes_a_real_revisit() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let real = root.join("real");
    fs::create_dir(&real).unwrap();
    for i in 0..ENTRIES {
        fs::File::create(real.join(format!("f{i:05}"))).unwrap();
    }
    std::os::unix::fs::symlink(&real, root.join("link")).unwrap();

    let opt = Options {
        follow_links: true,
        compute_physical: false,
        ..Default::default()
    };
    opt.dir_yield_every.store(32, Ordering::Relaxed);
    let map = scan_directory(&root, &opt).unwrap();
    let total = map.get(&root).map(|s| s.files).unwrap_or(0);
    assert_eq!(
        total as usize, ENTRIES,
        "the symlinked directory must be counted once, not twice"
    );
}
