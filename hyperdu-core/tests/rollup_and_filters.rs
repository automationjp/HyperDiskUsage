use hyperdu_core::{scan_directory, OptionsBuilder};
use std::fs;
use std::io::Write;
use std::path::PathBuf;

fn write_bytes(p: &std::path::Path, n: usize) {
    let mut f = fs::File::create(p).unwrap();
    f.write_all(&vec![b'x'; n]).unwrap();
}

#[test]
#[ignore = "Enable after stabilizing platform backends in CI"]
fn rollup_propagates_sizes() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    fs::create_dir(root.join("a")).unwrap();
    fs::create_dir(root.join("b")).unwrap();
    write_bytes(&root.join("a/f1"), 10);
    write_bytes(&root.join("b/f2"), 20);

    let opt = OptionsBuilder::new()
        .compute_physical(false)
        .approximate_sizes(true)
        .build();
    let map = scan_directory(&root, &opt).unwrap();
    let stat = map.get(&root).cloned().unwrap();
    assert_eq!(stat.files, 2, "two files counted");
}

#[test]
#[ignore = "Enable after stabilizing platform backends in CI"]
fn exclude_contains_excludes_dirs() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    fs::create_dir(root.join("a")).unwrap();
    fs::create_dir(root.join("b")).unwrap();
    write_bytes(&root.join("a/f1"), 10);
    write_bytes(&root.join("b/f2"), 20);

    let opt = OptionsBuilder::new()
        .with_exclude_contains(["b".to_string()])
        .compute_physical(false)
        .approximate_sizes(true)
        .build();
    let map = scan_directory(&root, &opt).unwrap();
    let stat = map.get(&root).cloned().unwrap();
    assert_eq!(stat.files, 1);
}

#[test]
#[ignore = "Enable after stabilizing platform backends in CI"]
fn min_file_size_filters_small_files() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    fs::create_dir(root.join("a")).unwrap();
    write_bytes(&root.join("a/f_small"), 10);
    write_bytes(&root.join("a/f_big"), 20);

    let opt = OptionsBuilder::new()
        .min_file_size(15)
        .compute_physical(false)
        .approximate_sizes(true)
        .build();
    let map = scan_directory(&root, &opt).unwrap();
    let stat = map.get(&root).cloned().unwrap();
    assert_eq!(stat.files, 1);
}

#[test]
#[ignore = "Enable after stabilizing platform backends in CI"]
fn max_depth_limits_grandchildren() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    fs::create_dir_all(root.join("child/grand")).unwrap();
    write_bytes(&root.join("child/f1"), 10);
    write_bytes(&root.join("child/grand/f2"), 20);

    // Depth semantics: 0 = unlimited; depth starts at 0 for root.
    // max_depth=1 scans root (0) and child (1), but not grandchild (2).
    let opt = OptionsBuilder::new()
        .max_depth(1)
        .compute_physical(false)
        .approximate_sizes(true)
        .build();
    let map = scan_directory(&root, &opt).unwrap();
    let stat = map.get(&root).cloned().unwrap();
    assert_eq!(
        stat.files, 1,
        "grandchild content should be excluded at max_depth=1"
    );
}

#[cfg(unix)]
#[test]
#[ignore = "Enable after stabilizing platform backends in CI"]
fn symlink_not_followed_by_default() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    let root: PathBuf = dir.path().to_path_buf();
    fs::create_dir(root.join("d")).unwrap();
    write_bytes(&root.join("d/file"), 10);
    // Create a symlink to the file at the root
    symlink(root.join("d/file"), root.join("link_to_file")).unwrap();

    // Default follow_links=false; symlink should not be counted
    let opt = OptionsBuilder::new()
        .compute_physical(false)
        .approximate_sizes(true)
        .build();
    let map = scan_directory(&root, &opt).unwrap();
    let stat = map.get(&root).cloned().unwrap();
    assert_eq!(stat.files, 1, "symlink should not be counted by default");
}
