//! A directory past the large-directory threshold has its `statx` calls spread
//! over the workers. The totals must not depend on how many there are.
#![cfg(target_os = "linux")]

use hyperdu_core::{scan_directory, CompatMode, Options};

fn totals(root: &std::path::Path, threads: usize, mode: CompatMode) -> (u64, u64, u64) {
    let opt = Options {
        threads,
        compat_mode: mode,
        ..Options::default()
    };
    let map = scan_directory(root, &opt).unwrap();
    let s = map[root];
    (s.files, s.logical, s.physical)
}

#[test]
fn large_directory_totals_do_not_depend_on_worker_count() {
    let dir = tempfile::tempdir().unwrap();
    // Well past one batch (1024 files) and past the point a directory counts as large.
    let n = 9000usize;
    let mut logical = 0u64;
    for i in 0..n {
        let len = (i % 7) * 100;
        std::fs::write(dir.path().join(format!("f{i:05}")), vec![b'x'; len]).unwrap();
        logical += len as u64;
    }
    std::fs::hard_link(dir.path().join("f00001"), dir.path().join("link")).unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    std::fs::write(dir.path().join("sub").join("g"), b"zz").unwrap();
    logical += 2;

    for mode in [CompatMode::HyperDU, CompatMode::GnuStrict] {
        let one = totals(dir.path(), 1, mode);
        let many = totals(dir.path(), 8, mode);
        assert_eq!(one, many, "{mode:?}");
        // The hardlink is counted once, the 9000 files and sub/g once each.
        assert_eq!(one.0, n as u64 + 1, "{mode:?}");
        assert_eq!(one.1, logical, "{mode:?}");
    }
}
