//! `Options::progress_counters` must finish every depth-1/2 directory it
//! starts, including when large directories are split into resume jobs.
use std::sync::{atomic::Ordering, Arc};

use hyperdu_core::{scan_directory, Options, ProgressCounters};

fn tree() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for d in ["a/x/deep/deeper", "a/y", "b"] {
        std::fs::create_dir_all(root.join(d)).unwrap();
    }
    for f in ["f", "a/f", "a/x/deep/deeper/f", "a/y/f", "b/f"] {
        std::fs::write(root.join(f), [0u8; 100]).unwrap();
    }
    // Enough entries in one depth-3 directory to be split into resume jobs.
    for i in 0..64 {
        std::fs::write(root.join(format!("a/x/deep/{i}")), b"1").unwrap();
    }
    dir
}

fn scan(yield_every: usize) -> Arc<ProgressCounters> {
    let dir = tree();
    let counters = Arc::new(ProgressCounters::default());
    let opt = Options {
        progress_counters: Some(counters.clone()),
        threads: 4,
        ..Options::default()
    };
    opt.dir_yield_every.store(yield_every, Ordering::Relaxed);
    let map = scan_directory(dir.path(), &opt).unwrap();
    assert_eq!(map.get(dir.path()).map(|s| s.files), Some(69));
    counters
}

#[test]
fn every_depth_one_and_two_directory_completes() {
    for yield_every in [0, 4] {
        let c = scan(yield_every);
        // a, b at depth 1; x, y at depth 2.
        assert_eq!(
            c.dirs_total.load(Ordering::Relaxed),
            4,
            "yield {yield_every}"
        );
        assert_eq!(
            c.dirs_done.load(Ordering::Relaxed),
            4,
            "yield {yield_every}"
        );
        assert!(c.bytes.load(Ordering::Relaxed) > 0);
    }
}
