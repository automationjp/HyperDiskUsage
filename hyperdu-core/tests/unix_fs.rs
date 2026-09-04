//! Unix integration tests against a real temp directory.
//!
//! These cover what size accounting on Unix has to get right, and exist because
//! a mutation test showed the whole suite stayed green after the sparse-file bug
//! (`blocks == 0` falling back to the logical size) was put back into
//! `calculate_physical_size`. Physical-size accounting on Unix had no coverage
//! at all; the Windows equivalents live in `windows_fs.rs`.
#![cfg(unix)]
#![allow(clippy::field_reassign_with_default)]

use std::{fs, io::Write, path::Path};

use hyperdu_core::{scan_directory, Options, Stat, StatMap};

fn quiet_opts() -> Options {
    let mut opt = Options::default();
    opt.progress_every = 0;
    opt.threads = 2;
    opt
}

fn write_bytes(p: &Path, n: usize) {
    let mut f = fs::File::create(p).unwrap();
    f.write_all(&vec![b'x'; n]).unwrap();
}

fn stat_of(map: &StatMap, p: &Path) -> Stat {
    *map.get(p)
        .unwrap_or_else(|| panic!("no entry for {}", p.display()))
}

/// Create a file that declares `len` bytes without allocating any blocks.
/// Returns `None` when the filesystem materialised it anyway, in which case the
/// caller must skip: the assertion would be testing the filesystem, not us.
fn make_sparse(path: &Path, len: u64) -> Option<()> {
    use std::os::unix::fs::MetadataExt;

    let f = fs::File::create(path).unwrap();
    f.set_len(len).unwrap();
    drop(f);
    (fs::metadata(path).unwrap().blocks() == 0).then_some(())
}

/// A file with no blocks allocated occupies no disk space, however large it
/// claims to be. Reporting its logical size as physical is what `du` would call
/// wrong, and it is an easy mistake to reintroduce: the "0 blocks must mean the
/// lookup failed" reading is plausible and incorrect.
#[test]
fn sparse_file_costs_no_physical_bytes() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("sparse");
    fs::create_dir_all(&root).unwrap();

    const LEN: u64 = 64 * 1024 * 1024;
    if make_sparse(&root.join("hole.bin"), LEN).is_none() {
        eprintln!("filesystem does not create sparse files here; skipping");
        return;
    }

    let s = stat_of(&scan_directory(&root, &quiet_opts()).unwrap(), &root);
    assert_eq!(s.files, 1);
    assert_eq!(s.logical, LEN, "logical size is what the file declares");
    assert_eq!(s.physical, 0, "a file with zero blocks occupies zero bytes");
}

/// The opposite direction: an ordinary file must report the space its blocks
/// actually take, which is rounded up to the block size and so is normally
/// larger than its logical size. Physical equal to logical here would mean the
/// blocks were never consulted.
#[test]
fn small_file_physical_size_is_rounded_up_to_blocks() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("small");
    fs::create_dir_all(&root).unwrap();
    write_bytes(&root.join("a.bin"), 10);

    let s = stat_of(&scan_directory(&root, &quiet_opts()).unwrap(), &root);
    assert_eq!(s.files, 1);
    assert_eq!(s.logical, 10);
    assert!(
        s.physical >= 512,
        "10 bytes still occupies at least one block, got physical={}",
        s.physical
    );
}

/// With `compute_physical` off the logical size stands in for the physical one,
/// so a sparse file reports its declared size. This is the one case where
/// physical == logical is the correct answer.
#[test]
fn logical_only_mode_reports_declared_size() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("logical");
    fs::create_dir_all(&root).unwrap();

    const LEN: u64 = 8 * 1024 * 1024;
    if make_sparse(&root.join("hole.bin"), LEN).is_none() {
        eprintln!("filesystem does not create sparse files here; skipping");
        return;
    }

    let mut opt = quiet_opts();
    opt.compute_physical = false;
    let s = stat_of(&scan_directory(&root, &opt).unwrap(), &root);
    assert_eq!(s.logical, LEN);
    assert_eq!(
        s.physical, s.logical,
        "logical-only mode substitutes the logical size"
    );
}

/// `min_file_size` filters on the logical size, and a filtered-out file must not
/// contribute its blocks either.
#[test]
fn min_file_size_excludes_both_sizes() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("minsize");
    fs::create_dir_all(&root).unwrap();
    write_bytes(&root.join("small.bin"), 10);
    write_bytes(&root.join("big.bin"), 4096);

    let mut opt = quiet_opts();
    opt.min_file_size = 1024;
    let s = stat_of(&scan_directory(&root, &opt).unwrap(), &root);
    assert_eq!(s.files, 1, "only big.bin counts");
    assert_eq!(s.logical, 4096);
    assert!(s.physical >= 4096);
}

/// Nothing is excluded unless the caller asks. A default scan that quietly
/// dropped `.github` (which `.git` matches as a substring) misreported where the
/// space went, and gave every comparison against du a head start.
#[test]
fn nothing_is_excluded_by_default() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("filters");
    fs::create_dir_all(root.join(".github").join("workflows")).unwrap();
    fs::create_dir_all(root.join(".git")).unwrap();
    write_bytes(&root.join(".github").join("workflows").join("ci.yml"), 100);
    write_bytes(&root.join(".git").join("HEAD"), 50);
    write_bytes(&root.join("README"), 25);

    let s = stat_of(&scan_directory(&root, &quiet_opts()).unwrap(), &root);
    assert_eq!(s.files, 3, "every file is counted by default");
    assert_eq!(s.logical, 175);

    // An explicit filter still excludes, and still matches by substring.
    let mut opt = quiet_opts();
    opt.exclude_contains = vec![".git".into()];
    let s = stat_of(&scan_directory(&root, &opt).unwrap(), &root);
    assert_eq!(s.files, 1, "'.git' matches '.github' too, by substring");
    assert_eq!(s.logical, 25);
}
