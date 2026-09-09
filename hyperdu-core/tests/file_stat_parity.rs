//! `file_stat` exists so a caller that enumerates a directory itself gets the
//! numbers the scan would have produced. That is only worth anything if the two
//! actually agree, so this compares them directly.

use std::fs;

use hyperdu_core::{compile_filters_in_place, file_stat, is_excluded, scan_directory, Options};

/// Sizes that straddle a cluster boundary on every common cluster size, so the
/// assertion is not satisfied by a filesystem that happens not to round.
const SIZES: [usize; 5] = [1, 4095, 4096, 10240, 70000];

#[test]
fn file_stat_matches_what_the_scan_reports() {
    for size in SIZES {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let path = root.join("f.bin");
        fs::write(&path, vec![b'x'; size]).unwrap();

        let opt = Options {
            compute_physical: true,
            ..Default::default()
        };
        let scanned = scan_directory(root, &opt).unwrap();
        let from_scan = scanned.get(root).expect("the root is in the map");
        let direct = file_stat(&path, &opt).expect("a regular file has a stat");

        assert_eq!(
            direct.logical, from_scan.logical,
            "logical size disagrees for a {size}-byte file"
        );
        assert_eq!(
            direct.physical, from_scan.physical,
            "physical size disagrees for a {size}-byte file"
        );
        assert_eq!(direct.logical, size as u64);
    }
}

/// With `compute_physical` off the scan reports the logical size, and so must
/// this. Guards against always asking the filesystem for the allocated size.
#[test]
fn file_stat_honours_compute_physical() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let path = root.join("f.bin");
    fs::write(&path, vec![b'x'; 10240]).unwrap();

    let opt = Options {
        compute_physical: false,
        ..Default::default()
    };
    let direct = file_stat(&path, &opt).unwrap();
    assert_eq!(direct.physical, direct.logical);
    assert_eq!(direct.logical, 10240);
}

#[test]
fn file_stat_declines_anything_that_is_not_a_regular_file() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let opt = Options::default();

    assert!(file_stat(root, &opt).is_none(), "a directory is not a file");
    assert!(
        file_stat(&root.join("does-not-exist"), &opt).is_none(),
        "a missing path has no stat"
    );
}

/// `is_excluded` reads the compiled matchers, which a hand-built `Options` does
/// not have until `compile_filters_in_place` runs. A caller that skips it would
/// silently get "nothing is excluded" for glob and regex patterns.
#[test]
fn is_excluded_needs_the_filters_compiled() {
    let mut opt = Options {
        exclude_glob: vec!["*.log".into()],
        ..Default::default()
    };
    let path = std::path::Path::new("/tmp/app.log");

    assert!(
        !is_excluded(path, &opt),
        "uncompiled options cannot match a glob; this documents the requirement"
    );

    compile_filters_in_place(&mut opt);
    assert!(is_excluded(path, &opt), "compiled glob must match");
}

#[test]
fn is_excluded_matches_the_entry_name_not_the_whole_path() {
    let mut opt = Options {
        exclude_contains: vec!["build".into()],
        ..Default::default()
    };
    compile_filters_in_place(&mut opt);

    assert!(is_excluded(std::path::Path::new("/src/build"), &opt));
    // The pattern appears in an ancestor, not in this entry's own name. Matching
    // the full path here would exclude everything under a root whose path
    // happens to contain the pattern.
    assert!(!is_excluded(std::path::Path::new("/build/src"), &opt));
}
