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

/// Hardlinks are counted once, like GNU du. The dedupe path is only entered for
/// files whose link count says they could be hardlinks, so this also guards the
/// `nlink > 1` gate: were the gate to skip a real hardlink, the total would
/// double.
#[test]
fn hardlinks_are_counted_once() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("links");
    fs::create_dir_all(&root).unwrap();
    write_bytes(&root.join("original.bin"), 4096);
    fs::hard_link(root.join("original.bin"), root.join("alias.bin")).unwrap();
    // A second, unlinked file makes the nlink == 1 path part of the same scan.
    write_bytes(&root.join("solo.bin"), 4096);

    let s = stat_of(&scan_directory(&root, &quiet_opts()).unwrap(), &root);
    assert_eq!(s.files, 2, "the alias is not counted a second time");
    assert_eq!(s.logical, 8192, "4096 for the shared inode plus 4096 solo");
}

/// With `count_hardlinks` on, every link is its own file. This is the non-GNU
/// mode, and the gate must not silently suppress it.
#[test]
fn hardlinks_are_counted_separately_when_requested() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("links_counted");
    fs::create_dir_all(&root).unwrap();
    write_bytes(&root.join("original.bin"), 4096);
    fs::hard_link(root.join("original.bin"), root.join("alias.bin")).unwrap();

    let mut opt = quiet_opts();
    opt.count_hardlinks = true;
    let s = stat_of(&scan_directory(&root, &opt).unwrap(), &root);
    assert_eq!(s.files, 2, "both links count");
    assert_eq!(s.logical, 8192);
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

/// Run each backend in a separate process: environment overrides must not race
/// other tests, and native CI must exercise GALB rather than silently falling back.
#[cfg(target_os = "macos")]
mod macos_bulk {
    use std::{
        os::unix::fs::{symlink, MetadataExt},
        process::Command,
        sync::{atomic::Ordering, Arc, Mutex},
    };

    use super::*;

    #[test]
    fn bulk_and_portable_backends_agree_on_native_filesystem() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("scan");
        fs::create_dir_all(root.join("nested/deep")).unwrap();
        for i in 0..160 {
            write_bytes(
                &root.join(format!("file-{i:03}-{}", "long-name-".repeat(8))),
                i + 1,
            );
        }
        // Neither backend may classify special files as regular files.
        let fifo = std::ffi::CString::new(std::os::unix::ffi::OsStrExt::as_bytes(
            root.join("fifo").as_os_str(),
        ))
        .unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        let _listener = std::os::unix::net::UnixListener::bind(root.join("socket")).unwrap();
        let original = root.join(format!("file-000-{}", "long-name-".repeat(8)));
        fs::hard_link(&original, root.join("hardlink")).unwrap();
        // TOTALSIZE would incorrectly include this resource fork in logical bytes.
        write_bytes(&original.join("..namedfork/rsrc"), 8192);
        fs::File::create(root.join("sparse"))
            .unwrap()
            .set_len(8 * 1024 * 1024)
            .unwrap();
        write_bytes(&root.join("nested/data"), 17);
        write_bytes(&root.join("nested/deep/data"), 19);
        write_bytes(&tmp.path().join("external-file"), 7);
        fs::create_dir(tmp.path().join("external-dir")).unwrap();
        write_bytes(&tmp.path().join("external-dir/data"), 11);
        symlink(tmp.path().join("external-file"), root.join("file-link")).unwrap();
        symlink(tmp.path().join("external-dir"), root.join("dir-link")).unwrap();
        symlink(&root, tmp.path().join("external-dir/cycle")).unwrap();

        let mut results = Vec::new();
        for mode in ["1", "0"] {
            let output = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "macos_bulk::subprocess_fixture", "--nocapture"])
                .env("HYPERDU_MAC_TEST_ROOT", &root)
                .env("HYPERDU_MAC_USE_GALB", mode)
                .env("HYPERDU_GALB_BUF_KB", "4")
                .output()
                .unwrap();
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                output.status.success(),
                "mode={mode}\n{stdout}\n{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let summary: Vec<_> = stdout
                .lines()
                .filter(|line| line.starts_with("MAC_RESULT "))
                .map(str::to_owned)
                .collect();
            assert_eq!(summary.len(), 6, "missing child results: {stdout}");
            results.push(summary);
        }
        assert_eq!(results[0], results[1]);
    }

    #[test]
    fn followed_broken_link_reports_the_entry_error() {
        let tmp = tempfile::tempdir().unwrap();
        symlink(tmp.path().join("absent"), tmp.path().join("broken")).unwrap();
        let mut opt = quiet_opts();
        opt.follow_links = true;
        let reports = Arc::new(Mutex::new(Vec::new()));
        let captured = reports.clone();
        opt.error_report = Some(Arc::new(move |message| {
            captured.lock().unwrap().push(message.to_owned())
        }));
        let s = stat_of(&scan_directory(tmp.path(), &opt).unwrap(), tmp.path());
        assert_eq!(s.files, 0);
        assert!(opt.error_count.load(Ordering::Relaxed) > 0);
        assert!(reports
            .lock()
            .unwrap()
            .iter()
            .any(|message| message.contains("broken")));
    }
    #[test]
    fn subprocess_fixture() {
        let Some(root) = std::env::var_os("HYPERDU_MAC_TEST_ROOT") else {
            return;
        };
        let root = std::path::PathBuf::from(root);
        let expected_logical = (1..=160).sum::<u64>() + 8 * 1024 * 1024 + 17 + 19;
        let mut expected_physical = 0;
        for item in fs::read_dir(&root).unwrap() {
            let item = item.unwrap();
            let name = item.file_name();
            if name.to_string_lossy().starts_with("file-0")
                || name.to_string_lossy().starts_with("file-1")
                || name == "sparse"
            {
                expected_physical += fs::symlink_metadata(item.path()).unwrap().blocks() * 512;
            }
        }
        expected_physical += fs::metadata(root.join("nested/data")).unwrap().blocks() * 512;
        expected_physical += fs::metadata(root.join("nested/deep/data"))
            .unwrap()
            .blocks()
            * 512;
        let original = root.join(format!("file-000-{}", "long-name-".repeat(8)));
        let expected_file = hyperdu_core::file_stat(&original, &quiet_opts()).unwrap();
        assert_eq!(expected_file.logical, 1);
        assert!(
            fs::metadata(original.join("..namedfork/rsrc"))
                .unwrap()
                .len()
                > expected_file.logical
        );
        for scenario in 0..6 {
            let mut opt = quiet_opts();
            opt.threads = 1;
            match scenario {
                1 => opt.compute_physical = false,
                2 => opt.count_hardlinks = true,
                3 => {
                    opt.follow_links = true;
                    opt.one_file_system = true;
                }
                4 => {
                    opt.exclude_contains = vec!["nested/".into()];
                    opt.min_file_size = 100;
                }
                5 => opt.max_depth = 1,
                _ => {}
            }
            let samples = Arc::new(Mutex::new(Vec::new()));
            let captured = samples.clone();
            opt.progress_every = 1;
            opt.progress_sample_callback = Some(Arc::new(move |sample| {
                captured.lock().unwrap().push((
                    sample.path.to_path_buf(),
                    sample.logical,
                    sample.physical,
                ));
            }));
            let s = stat_of(&scan_directory(&root, &opt).unwrap(), &root);
            assert_eq!(
                opt.error_count.load(Ordering::Relaxed),
                0,
                "backend must not hide a bulk error"
            );
            if scenario == 0 {
                assert_eq!(
                    (s.logical, s.physical, s.files),
                    (expected_logical, expected_physical, 163)
                );
            } else if scenario == 1 {
                assert_eq!(
                    (s.logical, s.physical),
                    (expected_logical, expected_logical)
                );
            } else if scenario == 2 {
                assert_eq!((s.logical, s.files), (expected_logical + 1, 164));
            } else if scenario == 3 {
                assert_eq!(
                    (s.logical, s.files),
                    (expected_logical + 18, 165),
                    "follow target sizes, traverse a directory link once and terminate its cycle"
                );
            } else if scenario == 4 {
                assert_eq!(
                    (s.logical, s.files),
                    ((100..=160).sum::<u64>() + 8 * 1024 * 1024, 62)
                );
            }
            if scenario == 5 {
                assert_eq!((s.logical, s.files), (expected_logical - 19, 162));
            }
            let samples = samples.lock().unwrap();
            assert!(!samples.is_empty());
            for (path, logical, physical) in samples.iter() {
                let md = fs::metadata(path).unwrap();
                assert_eq!(*logical, md.len());
                assert_eq!(
                    *physical,
                    if opt.compute_physical {
                        md.blocks() * 512
                    } else {
                        md.len()
                    }
                );
            }
            println!(
                "MAC_RESULT {scenario} {} {} {}",
                s.logical, s.physical, s.files
            );
        }

        // One small GALB batch must observe cancellation before consuming this
        // directory. The fallback reports one file per batch, also bounded.
        let mut opt = quiet_opts();
        opt.threads = 1;
        opt.progress_every = 1;
        let cancel = opt.cancel.clone();
        opt.progress_callback = Some(Arc::new(move |_| cancel.store(true, Ordering::Relaxed)));
        let s = stat_of(&scan_directory(&root, &opt).unwrap(), &root);
        assert!(opt.cancel.load(Ordering::Relaxed));
        assert!(
            s.files > 0 && s.files < 163,
            "cancellation ignored within directory: {}",
            s.files
        );
    }
}
