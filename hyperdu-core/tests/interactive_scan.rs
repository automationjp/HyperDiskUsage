use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use hyperdu_core::{scan_directory, scan_directory_mode, Options, ScanEvent, ScanMode, StatMap};

fn collect(root: &Path, opt: &Options) -> anyhow::Result<(StatMap, Vec<&'static str>)> {
    let mut map = StatMap::default();
    let mut events = Vec::new();
    scan_directory_mode(root, opt, ScanMode::Interactive, |event| match event {
        ScanEvent::RootListed {
            root, direct_files, ..
        } => {
            map.insert(root, direct_files);
            events.push("root");
        }
        ScanEvent::ChildCompleted {
            root: child,
            map: child_map,
        } => {
            if let Some(stat) = child_map.get(&child) {
                let total = map.get_mut(root).unwrap();
                total.logical += stat.logical;
                total.physical += stat.physical;
                total.files += stat.files;
            }
            map.extend(child_map);
            events.push("child");
        }
        ScanEvent::Finished => events.push("finished"),
        ScanEvent::Cancelled => events.push("cancelled"),
        _ => panic!("unexpected batch fallback"),
    })?;
    Ok((map, events))
}

fn fixture() -> tempfile::TempDir {
    let temp = tempfile::tempdir().unwrap();
    for (name, size) in [
        ("root.bin", 111),
        ("a/a.bin", 222),
        ("a/deep/b.bin", 333),
        ("b/b.bin", 444),
        ("excluded/x.bin", 555),
    ] {
        let path = temp.path().join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, vec![1; size]).unwrap();
    }
    temp
}

#[test]
fn interactive_matches_batch_for_depth_filters_and_sizes() {
    let dir = fixture();
    for depth in [0, 1, 2, 3] {
        for min in [0, 200] {
            let opt = Options {
                max_depth: depth,
                min_file_size: min,
                exclude_contains: vec!["excluded".into()],
                threads: 2,
                ..Options::default()
            };
            let batch = scan_directory(dir.path(), &opt).unwrap();
            let (interactive, events) = collect(dir.path(), &opt).unwrap();
            assert_eq!(
                normalized(&batch),
                normalized(&interactive),
                "depth={depth}, min={min}"
            );
            assert_eq!(events.first(), Some(&"root"));
            assert_eq!(events.last(), Some(&"finished"));
        }
    }
}

#[test]
fn hardlinks_share_identity_across_root_and_child_phases() {
    let dir = fixture();
    let source = dir.path().join("root.bin");
    std::fs::hard_link(&source, dir.path().join("a/link.bin")).unwrap();
    std::fs::hard_link(&source, dir.path().join("b/link.bin")).unwrap();
    for count in [false, true] {
        let opt = Options {
            count_hardlinks: count,
            threads: 2,
            ..Options::default()
        };
        let batch = scan_directory(dir.path(), &opt).unwrap();
        let (interactive, _) = collect(dir.path(), &opt).unwrap();
        assert_eq!(
            normalized(&batch).get(dir.path()),
            normalized(&interactive).get(dir.path())
        );
    }
}

#[test]
fn cancellation_during_child_has_no_false_completion() {
    let dir = fixture();
    let cancel = Arc::new(AtomicBool::new(false));
    let callback_cancel = cancel.clone();
    let opt = Options {
        cancel,
        threads: 1,
        progress_every: 1,
        progress_callback: Some(Arc::new(move |n| {
            if n >= 2 {
                callback_cancel.store(true, Ordering::Relaxed);
            }
        })),
        ..Options::default()
    };
    let (_, events) = collect(dir.path(), &opt).unwrap();
    assert_eq!(events, vec!["root", "cancelled"]);
}

#[test]
fn cancellation_before_start_and_missing_root_are_distinct() {
    let dir = fixture();
    let opt = Options {
        cancel: Arc::new(AtomicBool::new(true)),
        ..Options::default()
    };
    let (_, events) = collect(dir.path(), &opt).unwrap();
    assert_eq!(events, vec!["cancelled"]);
    assert!(collect(&dir.path().join("missing"), &Options::default()).is_err());
}

#[test]
fn batch_mode_delivers_a_whole_map() {
    let dir = fixture();
    let mut events = Vec::new();
    scan_directory_mode(
        dir.path(),
        &Options::default(),
        ScanMode::Batch,
        |event| match event {
            ScanEvent::BatchCompleted { map } => {
                assert!(map.contains_key(dir.path()));
                events.push("batch");
            }
            ScanEvent::Finished => events.push("finished"),
            _ => panic!("unexpected event"),
        },
    )
    .unwrap();
    assert_eq!(events, vec!["batch", "finished"]);
}

#[test]
fn root_resume_keeps_all_directories_and_direct_files() {
    let dir = fixture();
    for i in 0..100 {
        std::fs::write(dir.path().join(format!("file{i}")), [1; 32]).unwrap();
    }
    let opt = Options {
        threads: 2,
        ..Options::default()
    };
    opt.dir_yield_every.store(8, Ordering::Relaxed);
    assert_eq!(
        normalized(&scan_directory(dir.path(), &opt).unwrap()),
        normalized(&collect(dir.path(), &opt).unwrap().0)
    );
}

#[cfg(unix)]
#[test]
fn followed_cycle_uses_one_shared_visited_set() {
    let dir = fixture();
    std::os::unix::fs::symlink(dir.path(), dir.path().join("a/back")).unwrap();
    let opt = Options {
        follow_links: true,
        threads: 2,
        ..Options::default()
    };
    let batch = scan_directory(dir.path(), &opt).unwrap();
    let interactive = collect(dir.path(), &opt).unwrap().0;
    assert_eq!(
        normalized(&batch).get(dir.path()),
        normalized(&interactive).get(dir.path())
    );
}

fn normalized(map: &StatMap) -> std::collections::BTreeMap<PathBuf, (u64, u64, u64)> {
    map.iter()
        .map(|(path, s)| (path.clone(), (s.logical, s.physical, s.files)))
        .collect()
}

#[test]
fn progress_is_cumulative_across_child_boundaries() {
    let dir = fixture();
    let values = Arc::new(std::sync::Mutex::new(Vec::new()));
    let received = values.clone();
    let opt = Options {
        threads: 1,
        progress_every: 1,
        progress_callback: Some(Arc::new(move |n| received.lock().unwrap().push(n))),
        ..Options::default()
    };
    let (map, _) = collect(dir.path(), &opt).unwrap();
    let values = values.lock().unwrap();
    assert!(values.windows(2).all(|pair| pair[0] <= pair[1]));
    assert_eq!(
        values.last().copied(),
        Some(map.get(dir.path()).unwrap().files)
    );
}

#[test]
fn unavailable_mft_retains_interactive_enumeration() {
    let dir = fixture();
    let opt = Options {
        use_mft: true,
        ..Options::default()
    };
    let (_, events) = collect(dir.path(), &opt).unwrap();
    assert_eq!(events.first(), Some(&"root"));
    assert_eq!(events.last(), Some(&"finished"));
}

#[cfg(target_os = "linux")]
#[test]
fn one_file_system_rejects_a_link_into_another_filesystem() {
    use std::os::unix::fs::MetadataExt;
    if !Path::new("/dev/shm").is_dir() {
        return;
    }
    let dir = fixture();
    let other = tempfile::tempdir_in("/dev/shm").unwrap();
    if dir.path().metadata().unwrap().dev() == other.path().metadata().unwrap().dev() {
        return;
    }
    std::fs::write(other.path().join("other.bin"), [1; 1000]).unwrap();
    std::os::unix::fs::symlink(other.path(), dir.path().join("a/cross-volume")).unwrap();
    let opt = Options {
        follow_links: true,
        one_file_system: true,
        threads: 2,
        ..Options::default()
    };
    let batch = scan_directory(dir.path(), &opt).unwrap();
    let interactive = collect(dir.path(), &opt).unwrap().0;
    assert_eq!(
        normalized(&batch).get(dir.path()),
        normalized(&interactive).get(dir.path())
    );
    assert_eq!(interactive.get(dir.path()).unwrap().files, 5);
}

#[test]
fn cancellation_from_batch_result_has_no_false_completion() {
    let dir = fixture();
    let opt = Options::default();
    let mut events = Vec::new();
    scan_directory_mode(dir.path(), &opt, ScanMode::Batch, |event| match event {
        ScanEvent::BatchCompleted { .. } => {
            events.push("batch");
            opt.cancel.store(true, Ordering::Relaxed);
        }
        ScanEvent::Cancelled => events.push("cancelled"),
        ScanEvent::Finished => events.push("finished"),
        _ => panic!("unexpected event"),
    })
    .unwrap();
    assert_eq!(events, ["batch", "cancelled"]);
}
