//! Real filesystem parity for opt-in Linux metadata backends.
//! XFS requires an explicitly supplied owned read-only fixture mount.
#![cfg(all(target_os = "linux", target_arch = "x86_64", target_env = "gnu"))]

use std::{
    collections::BTreeMap,
    ffi::CString,
    fs,
    io::{Seek, SeekFrom, Write},
    os::unix::{ffi::OsStrExt, fs::symlink, net::UnixListener},
    path::{Path, PathBuf},
    sync::{atomic::Ordering, Arc},
};

use hyperdu_core::{scan_directory, CompatMode, Options};

fn scan(root: &Path, opt: &Options) -> BTreeMap<PathBuf, (u64, u64, u64)> {
    let result = scan_directory(root, opt).unwrap();
    assert_eq!(opt.error_count.load(Ordering::Relaxed), 0);
    result
        .iter()
        .map(|(path, stat)| {
            (
                path.strip_prefix(root).unwrap().to_path_buf(),
                (stat.logical, stat.physical, stat.files),
            )
        })
        .collect()
}

fn options() -> Options {
    Options {
        threads: 1,
        progress_every: 0,
        ..Options::default()
    }
}

#[test]
fn accelerated_scans_preserve_filters_hardlinks_special_files_and_resume() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("tree");
    fs::create_dir_all(root.join("nested/deep")).unwrap();
    for index in 0..145 {
        fs::write(root.join(format!("file-{index}")), vec![1; index + 1]).unwrap();
    }
    let sparse = root.join("sparse");
    let mut file = fs::File::create(&sparse).unwrap();
    file.set_len(8 * 1024 * 1024).unwrap();
    file.seek(SeekFrom::Start(4096)).unwrap();
    file.write_all(b"allocated portion").unwrap();
    fs::hard_link(&sparse, root.join("sparse-alias")).unwrap();
    fs::write(root.join("nested/deep/data"), [7; 53]).unwrap();
    // Keep aliases in their own directory to make parent attribution deterministic.
    fs::write(tmp.path().join("external"), [9; 17]).unwrap();
    symlink(tmp.path().join("external"), root.join("link")).unwrap();
    symlink(&root, root.join("nested/cycle")).unwrap();
    let fifo = CString::new(root.join("fifo").as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    let _socket = UnixListener::bind(root.join("socket")).unwrap();
    for mode in [
        CompatMode::HyperDU,
        CompatMode::GnuStrict,
        CompatMode::PosixStrict,
    ] {
        for variant in 0..7 {
            let mut baseline = options();
            baseline.compat_mode = mode;
            match variant {
                1 => baseline.count_hardlinks = true,
                2 => baseline.follow_links = true,
                3 => baseline.min_file_size = 50,
                4 => baseline.max_depth = 1,
                5 => baseline.exclude_contains = vec!["nested/".into(), "file-1".into()],
                6 => baseline.compute_physical = false,
                _ => {}
            }
            let expected = scan(&root, &baseline);
            for (uring, xfs) in [(true, false), (false, true), (true, true)] {
                let mut accelerated = baseline.clone();
                accelerated.use_io_uring = uring;
                accelerated.use_xfs_bulk = xfs;
                accelerated.dir_yield_every = Arc::new(79.into());
                assert_eq!(
                    scan(&root, &accelerated),
                    expected,
                    "variant={variant} uring={uring} xfs={xfs}"
                );
            }
        }
    }
}

#[test]
fn failed_followed_metadata_is_reported_and_cancellation_returns() {
    let tmp = tempfile::tempdir().unwrap();
    for index in 0..70 {
        fs::write(tmp.path().join(format!("file-{index}")), b"data").unwrap();
    }
    symlink("absent", tmp.path().join("broken")).unwrap();
    let mut opt = options();
    opt.use_io_uring = true;
    opt.follow_links = true;
    let _ = scan_directory(tmp.path(), &opt).unwrap();
    assert_eq!(opt.error_count.load(Ordering::Relaxed), 1);
    let mut cancelled = options();
    cancelled.use_io_uring = true;
    cancelled.cancel.store(true, Ordering::Relaxed);
    let result = scan_directory(tmp.path(), &cancelled);
    assert!(result.is_err() || result.unwrap().is_empty());
}

#[test]
fn owned_read_only_xfs_matches_live_metadata_for_every_directory() {
    let Some(root) = std::env::var_os("HYPERDU_TEST_XFS_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    for mode in [CompatMode::HyperDU, CompatMode::GnuStrict] {
        let mut baseline = options();
        baseline.compat_mode = mode;
        let expected = scan(&root, &baseline);
        for (uring, links, yield_every) in [(false, false, 0), (true, false, 79), (false, true, 79)]
        {
            baseline.count_hardlinks = links;
            let expected = if links {
                scan(&root, &baseline)
            } else {
                expected.clone()
            };
            let mut accelerated = baseline.clone();
            accelerated.use_xfs_bulk = true;
            accelerated.use_io_uring = uring;
            accelerated.dir_yield_every = Arc::new(yield_every.into());
            assert_eq!(scan(&root, &accelerated), expected);
        }
    }
}
