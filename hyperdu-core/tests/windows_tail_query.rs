//! The last directory query is skipped on local NTFS when a response leaves room.
//! Directories whose listing spans several ~64 KiB responses must still be
//! counted in full, including the subdirectories listed after the first response.
#![cfg(windows)]

use hyperdu_core::{scan_directory, Options};

#[test]
fn directories_larger_than_one_response_are_counted_in_full() {
    let dir = tempfile::tempdir().unwrap();
    // Short names pack hundreds of records per response; thousands of entries span several.
    let long = "n";
    let (files, subdirs) = (6000usize, 400usize);
    for i in 0..files {
        std::fs::write(dir.path().join(format!("{long}{i:05}")), b"x").unwrap();
    }
    for d in 0..subdirs {
        let sub = dir.path().join(format!("{long}d{d:03}"));
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("f"), b"yy").unwrap();
    }
    let map = scan_directory(dir.path(), &Options::default()).unwrap();
    let root = map.values().map(|s| s.files).max().unwrap();
    assert_eq!(root, (files + subdirs) as u64);
    assert_eq!(map.len(), subdirs + 1);
}
