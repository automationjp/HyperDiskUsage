//! Exercises the Win32 (`FindFirstFileExW`) enumeration backend.
//!
//! The backend choice is cached in a process-wide `OnceLock`, so this file holds
//! a single test and sets the environment variable before the first scan. Do not
//! add a test here that expects the default NT backend.
#![cfg(windows)]
#![allow(clippy::field_reassign_with_default)]

use std::{fs, io::Write, path::Path};

use hyperdu_core::{scan_directory, Options};

fn write_bytes(p: &Path, n: usize) {
    let mut f = fs::File::create(p).unwrap();
    f.write_all(&vec![b'x'; n]).unwrap();
}

#[test]
fn win32_backend_reports_the_same_totals() {
    std::env::set_var("HYPERDU_WIN_USE_NTQUERY", "0");

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("r");
    fs::create_dir_all(root.join("sub").join("deep")).unwrap();
    write_bytes(&root.join("a.bin"), 100);
    write_bytes(&root.join("sub").join("b.bin"), 200);
    write_bytes(&root.join("sub").join("deep").join("c.bin"), 300);
    fs::hard_link(root.join("a.bin"), root.join("link.bin")).unwrap();

    let mut opt = Options::default();
    opt.progress_every = 0;
    opt.threads = 4;
    opt.count_hardlinks = true;

    let map = scan_directory(&root, &opt).unwrap();
    let s = map.get(&root).copied().unwrap_or_default();
    assert_eq!(s.files, 4, "three files plus the hardlink: {map:?}");
    assert_eq!(s.logical, 700);
    assert!(s.physical >= s.logical, "allocation size covers the data");
    assert_eq!(map.get(&root.join("sub")).unwrap().files, 2);
    assert_eq!(map.get(&root.join("sub").join("deep")).unwrap().files, 1);
    assert!(
        !map.contains_key(tmp.path()),
        "no entry above the scan root"
    );

    // Hardlink dedupe on the same backend.
    let mut opt = opt.clone();
    opt.count_hardlinks = false;
    opt.inode_cache = Some(std::sync::Arc::new(dashmap::DashMap::with_capacity(16)));
    let map = scan_directory(&root, &opt).unwrap();
    let s = map.get(&root).copied().unwrap_or_default();
    assert_eq!(s.files, 3, "the second link is not counted again");
    assert_eq!(s.logical, 600);
}
