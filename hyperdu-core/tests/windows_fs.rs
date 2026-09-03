//! Windows integration tests against a real NTFS temp directory.
#![cfg(windows)]
#![allow(clippy::field_reassign_with_default)]

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use hyperdu_core::{scan_directory, Options, Stat};

fn write_bytes(p: &Path, n: usize) {
    let mut f = fs::File::create(p).unwrap();
    f.write_all(&vec![b'x'; n]).unwrap();
}

fn quiet_opts() -> Options {
    let mut opt = Options::default();
    opt.progress_every = 0;
    opt.threads = 4;
    opt
}

fn stat_of(map: &hyperdu_core::StatMap, p: &Path) -> Stat {
    map.get(p).copied().unwrap_or_default()
}

/// Layout: root/{a.bin:100, sub/{b.bin:200, deep/{c.bin:300}}, x.tmp:7}
fn build_tree(root: &Path) {
    fs::create_dir_all(root.join("sub").join("deep")).unwrap();
    write_bytes(&root.join("a.bin"), 100);
    write_bytes(&root.join("sub").join("b.bin"), 200);
    write_bytes(&root.join("sub").join("deep").join("c.bin"), 300);
    write_bytes(&root.join("x.tmp"), 7);
}

#[test]
fn counts_files_and_rolls_up_without_parent_of_root() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("r");
    build_tree(&root);
    let map = scan_directory(&root, &quiet_opts()).unwrap();
    let s = stat_of(&map, &root);
    assert_eq!(s.files, 4);
    assert_eq!(s.logical, 607);
    assert!(s.physical >= s.logical, "allocation size >= logical");
    assert_eq!(stat_of(&map, &root.join("sub")).files, 2);
    assert_eq!(stat_of(&map, &root.join("sub").join("deep")).files, 1);
    assert!(
        !map.contains_key(tmp.path()),
        "no entry above the scan root"
    );
}

#[test]
fn logical_only_reports_logical_as_physical() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("r");
    build_tree(&root);
    let mut opt = quiet_opts();
    opt.compute_physical = false;
    let map = scan_directory(&root, &opt).unwrap();
    let s = stat_of(&map, &root);
    assert_eq!(s.physical, 607);
}

#[test]
fn glob_and_regex_filters_apply_to_files() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("r");
    build_tree(&root);
    let mut opt = quiet_opts();
    opt.exclude_glob = vec!["**/*.tmp".into()];
    let map = scan_directory(&root, &opt).unwrap();
    assert_eq!(stat_of(&map, &root).files, 3, "x.tmp excluded by glob");

    let mut opt = quiet_opts();
    opt.exclude_regex = vec![r"b\.bin$".into()];
    let map = scan_directory(&root, &opt).unwrap();
    assert_eq!(stat_of(&map, &root).files, 3, "b.bin excluded by regex");
}

#[test]
fn name_filter_excludes_subdir_but_not_root() {
    let tmp = tempfile::tempdir().unwrap();
    // Root name contains the default exclude pattern "target".
    let root = tmp.path().join("my_target_root");
    build_tree(&root);
    fs::create_dir_all(root.join("targets")).unwrap();
    write_bytes(&root.join("targets").join("t.bin"), 50);
    let map = scan_directory(&root, &quiet_opts()).unwrap();
    let s = stat_of(&map, &root);
    assert_eq!(
        s.files, 4,
        "root itself is scanned; 'targets' subdir excluded"
    );
    assert!(!map.contains_key(&root.join("targets")));
}

#[test]
fn min_size_and_max_depth() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("r");
    build_tree(&root);
    let mut opt = quiet_opts();
    opt.min_file_size = 150;
    let map = scan_directory(&root, &opt).unwrap();
    assert_eq!(stat_of(&map, &root).files, 2);

    let mut opt = quiet_opts();
    opt.max_depth = 1; // root(0) + sub(1); deep(2) not entered
    let map = scan_directory(&root, &opt).unwrap();
    assert_eq!(stat_of(&map, &root).files, 3);
    assert!(!map.contains_key(&root.join("sub").join("deep")));
}

#[test]
fn hardlinks_deduped_only_when_requested() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("r");
    fs::create_dir_all(&root).unwrap();
    write_bytes(&root.join("a.bin"), 1000);
    fs::hard_link(root.join("a.bin"), root.join("b.bin")).unwrap();

    let mut opt = quiet_opts();
    opt.count_hardlinks = true;
    let map = scan_directory(&root, &opt).unwrap();
    assert_eq!(stat_of(&map, &root).files, 2);

    let mut opt = quiet_opts();
    opt.count_hardlinks = false;
    opt.inode_cache = Some(std::sync::Arc::new(dashmap::DashMap::with_capacity(16)));
    let map = scan_directory(&root, &opt).unwrap();
    let s = stat_of(&map, &root);
    assert_eq!(s.files, 1, "second link deduped");
    assert_eq!(s.logical, 1000);
}

#[test]
fn junction_is_skipped_unless_following_links() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("r");
    let target = tmp.path().join("t");
    fs::create_dir_all(&root).unwrap();
    fs::create_dir_all(&target).unwrap();
    write_bytes(&target.join("inside.bin"), 40);
    let link = root.join("jn");
    let status = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(&link)
        .arg(&target)
        .output();
    let created = matches!(&status, Ok(o) if o.status.success())
        && fs::symlink_metadata(&link)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false);
    if !created {
        eprintln!("skip: could not create junction: {status:?}");
        return;
    }
    let map = scan_directory(&root, &quiet_opts()).unwrap();
    assert_eq!(stat_of(&map, &root).files, 0, "junction not followed");

    let mut opt = quiet_opts();
    opt.follow_links = true;
    opt.error_report = Some(std::sync::Arc::new(|m: &str| eprintln!("scan error: {m}")));
    let map = scan_directory(&root, &opt).unwrap();
    assert_eq!(stat_of(&map, &root).files, 1, "junction followed: {map:?}");
}

/// A junction pointing at its own ancestor must not make the scan loop forever.
#[test]
fn junction_cycle_terminates_when_following_links() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("r");
    fs::create_dir_all(root.join("sub")).unwrap();
    write_bytes(&root.join("sub").join("f.bin"), 10);
    let link = root.join("sub").join("up");
    let status = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(&link)
        .arg(&root)
        .output();
    let created = matches!(&status, Ok(o) if o.status.success())
        && fs::symlink_metadata(&link)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false);
    if !created {
        eprintln!("skip: could not create junction: {status:?}");
        return;
    }
    let mut opt = quiet_opts();
    opt.follow_links = true;
    // No visited_dirs set here on purpose: the scanner must install one itself.
    let map = scan_directory(&root, &opt).unwrap();
    assert_eq!(
        stat_of(&map, &root).files,
        1,
        "each file counted once despite the cycle: {map:?}"
    );
}

/// A sparse file occupies no clusters, so its physical size must not be the
/// logical size. Enumeration reports allocation size 0 for it.
#[test]
fn sparse_file_costs_no_physical_bytes() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("r");
    fs::create_dir_all(&root).unwrap();
    let sparse = root.join("sparse.bin");
    let out = std::process::Command::new("fsutil")
        .args(["file", "createnew"])
        .arg(&sparse)
        .arg("1048576")
        .output();
    let created = matches!(&out, Ok(o) if o.status.success());
    if !created {
        eprintln!("skip: fsutil unavailable: {out:?}");
        return;
    }
    let flagged = std::process::Command::new("fsutil")
        .args(["sparse", "setflag"])
        .arg(&sparse)
        .output();
    if !matches!(&flagged, Ok(o) if o.status.success()) {
        eprintln!("skip: cannot set the sparse flag: {flagged:?}");
        return;
    }
    // Re-punch the range so the data is actually deallocated.
    let _ = std::process::Command::new("fsutil")
        .args(["sparse", "setrange"])
        .arg(&sparse)
        .args(["0", "1048576"])
        .output();

    let map = scan_directory(&root, &quiet_opts()).unwrap();
    let s = stat_of(&map, &root);
    assert_eq!(s.files, 1);
    assert_eq!(s.logical, 1_048_576, "logical size is the full length");
    assert!(
        s.physical < s.logical,
        "sparse data must not be billed as allocated: physical={}",
        s.physical
    );
}

#[test]
fn long_paths_beyond_max_path_are_scanned() {
    let tmp = tempfile::tempdir().unwrap();
    let mut deep: PathBuf = tmp.path().join("r");
    let seg = "0123456789abcdef0123456789abcdef"; // 32 chars
    for _ in 0..10 {
        deep.push(seg);
    }
    assert!(deep.as_os_str().len() > 300);
    fs::create_dir_all(&deep).unwrap();
    write_bytes(&deep.join("leaf.bin"), 123);
    let root = tmp.path().join("r");
    let map = scan_directory(&root, &quiet_opts()).unwrap();
    let s = stat_of(&map, &root);
    assert_eq!(s.files, 1);
    assert_eq!(s.logical, 123);
    assert_eq!(stat_of(&map, &deep).files, 1);
}

#[test]
fn forward_slash_root_keeps_input_spelling_in_keys() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("r");
    build_tree(&root);
    let slashed = PathBuf::from(root.to_string_lossy().replace('\\', "/"));
    let map = scan_directory(&slashed, &quiet_opts()).unwrap();
    assert_eq!(stat_of(&map, &slashed).files, 4);
    assert_eq!(stat_of(&map, &slashed.join("sub")).files, 2);
}

#[test]
fn progress_callback_reports_totals() {
    use std::sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    };
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("r");
    fs::create_dir_all(&root).unwrap();
    for i in 0..50 {
        write_bytes(&root.join(format!("f{i}")), 1);
    }
    let seen = Arc::new(AtomicU64::new(0));
    let seen2 = seen.clone();
    let mut opt = quiet_opts();
    opt.progress_every = 10;
    opt.progress_callback = Some(Arc::new(move |n| {
        seen2.fetch_max(n, Ordering::Relaxed);
    }));
    let map = scan_directory(&root, &opt).unwrap();
    assert_eq!(stat_of(&map, &root).files, 50);
    assert_eq!(
        seen.load(Ordering::Relaxed),
        50,
        "batched progress fired at the end"
    );
}
