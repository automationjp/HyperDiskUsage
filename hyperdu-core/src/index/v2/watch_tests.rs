use std::{
    collections::VecDeque,
    fs,
    sync::{atomic::Ordering, Arc, Mutex},
};

use super::*;
use crate::index::journal::{JournalCursor, JournalKind};

#[derive(Default)]
struct Events {
    batches: VecDeque<(Vec<Change>, bool)>,
    registered: Vec<EntryId>,
    removed: Vec<EntryId>,
}
struct Mock {
    cursor: JournalCursor,
    events: Arc<Mutex<Events>>,
}
impl NativeJournal for Mock {
    fn cursor(&self) -> JournalCursor {
        self.cursor
    }
    fn poll(&mut self) -> io::Result<Batch> {
        let (changes, caught_up) = self
            .events
            .lock()
            .unwrap()
            .batches
            .pop_front()
            .unwrap_or_else(|| (vec![], true));
        let from = self.cursor;
        self.cursor.position += 1;
        Ok(Batch {
            from,
            next: self.cursor,
            changes,
            caught_up,
        })
    }
    fn register_directory(&mut self, _: &Path, id: EntryId) -> io::Result<()> {
        self.events.lock().unwrap().registered.push(id);
        Ok(())
    }
    fn unregister_directory(&mut self, id: EntryId) -> io::Result<()> {
        self.events.lock().unwrap().removed.push(id);
        Ok(())
    }
}
fn start(root: &Path) -> (IndexWatcher, Arc<Mutex<Events>>) {
    let events = Arc::new(Mutex::new(Events::default()));
    let source = Mock {
        cursor: JournalCursor {
            kind: JournalKind::Inotify,
            volume: observe(root).unwrap().id.volume,
            epoch: 123,
            position: 0,
        },
        events: events.clone(),
    };
    let watcher = IndexWatcher::with_source(
        root.canonicalize().unwrap(),
        None,
        Box::new(source),
        false,
        &AtomicBool::new(false),
    )
    .unwrap();
    (watcher, events)
}
fn change(parent: EntryId, name: &str) -> Change {
    Change::Entry {
        key: LinkKey {
            parent,
            name: name.into(),
        },
        subtree: false,
    }
}
fn send(events: &Arc<Mutex<Events>>, changes: Vec<Change>) {
    events.lock().unwrap().batches.push_back((changes, true));
}
fn assert_baseline(watcher: &IndexWatcher) {
    let baseline = PersistentIndex::scan_snapshot(&watcher.root, &AtomicBool::new(false)).unwrap();
    assert_eq!(
        watcher.index.total(watcher.index.root()).unwrap().0,
        baseline.total(baseline.root()).unwrap().0
    );
    assert_eq!(watcher.index.file_count(), baseline.file_count());
    assert_eq!(watcher.index.directory_count(), baseline.directory_count());
}

#[test]
fn changed_file_coalesces_without_enumerating_directories() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("file"), [0; 11]).unwrap();
    let (mut watcher, events) = start(temp.path());
    let root = watcher.index.root();
    fs::write(temp.path().join("file"), [1; 8193]).unwrap();
    send(&events, vec![change(root, "file"), change(root, "file")]);
    let result = watcher.drain(&AtomicBool::new(false)).unwrap();
    assert!(result.caught_up);
    assert_eq!(result.stats.observed_entries, 1);
    assert_eq!(result.stats.directories_read, 0);
    assert_eq!(watcher.index.total(root).unwrap().1, Freshness::Observed);
    assert_baseline(&watcher);
}

#[test]
fn rename_keeps_descendants_and_reobserves_changed_children_at_new_parent() {
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir(temp.path().join("old")).unwrap();
    for number in 0..40 {
        fs::write(temp.path().join("old").join(number.to_string()), [0; 17]).unwrap();
    }
    let (mut watcher, events) = start(temp.path());
    let root = watcher.index.root();
    let dir = watcher.index.lookup(root, OsStr::new("old")).unwrap();
    fs::rename(temp.path().join("old"), temp.path().join("new")).unwrap();
    fs::write(temp.path().join("new/2"), [3; 9001]).unwrap();
    send(
        &events,
        vec![change(dir, "2"), change(root, "old"), change(root, "new")],
    );
    let result = watcher.drain(&AtomicBool::new(false)).unwrap();
    assert_eq!(result.stats.directories_read, 0);
    assert_eq!(watcher.index.lookup(root, OsStr::new("new")), Some(dir));
    assert!(events.lock().unwrap().removed.is_empty());
    assert_baseline(&watcher);
}

#[test]
fn arrived_subtree_and_deleted_subtree_register_and_release_only_affected_watches() {
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir_all(temp.path().join("removed/child")).unwrap();
    fs::write(temp.path().join("removed/child/f"), [0; 7]).unwrap();
    let (mut watcher, events) = start(temp.path());
    let root = watcher.index.root();
    let removed = observe(&temp.path().join("removed")).unwrap().id;
    let child = observe(&temp.path().join("removed/child")).unwrap().id;
    // Keep both trees alive while allocating: this test checks selective watch
    // removal, while the separate recycled-ID regression covers new incarnations.
    fs::create_dir_all(temp.path().join("arrived/inner")).unwrap();
    fs::write(temp.path().join("arrived/inner/g"), [0; 103]).unwrap();
    fs::remove_dir_all(temp.path().join("removed")).unwrap();
    let inner = observe(&temp.path().join("arrived/inner")).unwrap().id;
    send(
        &events,
        vec![
            change(inner, "g"),
            change(root, "removed"),
            change(root, "arrived"),
        ],
    );
    let result = watcher.drain(&AtomicBool::new(false)).unwrap();
    assert_eq!(result.stats.directories_read, 2);
    let released = events.lock().unwrap().removed.clone();
    assert!(released.contains(&removed) && released.contains(&child));
    assert!(!released.contains(&root));
    assert_baseline(&watcher);
}

#[test]
fn queue_requires_empty_barrier_and_busy_poll_never_publishes_observed() {
    let temp = tempfile::tempdir().unwrap();
    let (mut watcher, events) = start(temp.path());
    let root = watcher.index.root();
    for _ in 0..65 {
        send(&events, vec![change(root, "absent")]);
    }
    let first = watcher.drain(&AtomicBool::new(false)).unwrap();
    assert!(!first.caught_up);
    assert_eq!(watcher.index.total(root).unwrap().1, Freshness::Stale);
    assert!(watcher.drain(&AtomicBool::new(false)).unwrap().caught_up);
    let c = watcher.index.cursor.unwrap();
    let mut batch = Batch {
        from: c,
        next: c,
        changes: vec![],
        caught_up: true,
    };
    batch.next.epoch += 1;
    assert!(watcher.validate_batch(&batch).is_err());
    batch.next = c;
    batch.from.position -= 1;
    assert!(watcher.validate_batch(&batch).is_err());
    batch.from = c;
    batch.next.position -= 1;
    assert!(watcher.validate_batch(&batch).is_err());
}

#[test]
fn reset_root_replacement_and_cancellation_refuse_publication() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("root");
    fs::create_dir(&path).unwrap();
    let (mut watcher, events) = start(&path);
    send(&events, vec![Change::Reset("test overflow")]);
    assert!(watcher.drain(&AtomicBool::new(false)).is_err());
    assert_eq!(
        watcher.index.total(watcher.index.root).unwrap().1,
        Freshness::Stale
    );
    let cancel = AtomicBool::new(false);
    cancel.store(true, Ordering::Relaxed);
    assert_eq!(
        watcher.poll(&cancel).unwrap_err().kind(),
        io::ErrorKind::Interrupted
    );
    fs::rename(&path, temp.path().join("old")).unwrap();
    fs::create_dir(&path).unwrap();
    assert!(watcher.drain(&AtomicBool::new(false)).is_err());
    assert!(watcher.rebuild(&AtomicBool::new(false)).is_err());
}

#[test]
fn persistent_resume_stays_stale_until_replay_and_preserves_offline_changes() {
    let temp = tempfile::tempdir().unwrap();
    let root_path = temp.path().join("root");
    fs::create_dir(&root_path).unwrap();
    fs::write(root_path.join("file"), [0; 13]).unwrap();
    let (mut watcher, _) = start(&root_path);
    watcher.drain(&AtomicBool::new(false)).unwrap();
    let database = temp.path().join("snapshot");
    watcher.index.save(&database).unwrap();
    let saved = PersistentIndex::load(&database).unwrap();
    assert_eq!(saved.total(saved.root).unwrap().1, Freshness::Stale);
    let root = saved.root;
    let cursor = saved.cursor.unwrap();
    fs::write(root_path.join("file"), [0; 4111]).unwrap();
    let events = Arc::new(Mutex::new(Events::default()));
    send(&events, vec![change(root, "file")]);
    let source = Box::new(Mock {
        cursor,
        events: events.clone(),
    });
    let mut resumed = IndexWatcher::with_source(
        root_path,
        Some(saved),
        source,
        true,
        &AtomicBool::new(false),
    )
    .unwrap();
    assert!(events.lock().unwrap().registered.is_empty());
    let update = resumed.drain(&AtomicBool::new(false)).unwrap();
    assert_eq!(update.stats.directories_read, 0);
    assert_baseline(&resumed);
}

#[test]
fn relative_events_rescan_only_new_ancestor_and_reject_escaped_paths() {
    let temp = tempfile::tempdir().unwrap();
    let (mut watcher, events) = start(temp.path());
    fs::create_dir_all(temp.path().join("new/deep")).unwrap();
    fs::write(temp.path().join("new/deep/a"), [0; 31]).unwrap();
    send(
        &events,
        vec![Change::RelativePath {
            path: "new/deep/a".into(),
            subtree: false,
        }],
    );
    let update = watcher.drain(&AtomicBool::new(false)).unwrap();
    assert_eq!(update.stats.directories_read, 2);
    assert_baseline(&watcher);
    assert!(watcher.path_target(Path::new("../escape")).is_err());
    assert!(watcher
        .set_reconcile_interval(Duration::from_millis(1))
        .is_err());
}

#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
#[test]
fn native_service_updates_restarts_and_recovers_a_gap() {
    let temp = {
        #[cfg(windows)]
        {
            let Some(path) = std::env::var_os("HYPERDU_TEST_USN_ROOT") else {
                eprintln!("NOT RUN: native USN replay requires HYPERDU_TEST_USN_ROOT on an owned writable NTFS/ReFS fixture");
                return;
            };
            tempfile::tempdir_in(path).unwrap()
        }
        #[cfg(not(windows))]
        {
            tempfile::tempdir().unwrap()
        }
    };
    let root = temp.path().join("root");
    fs::create_dir_all(root.join("old/sub")).unwrap();
    fs::write(root.join("old/sub/file"), [0; 17]).unwrap();
    let cancel = AtomicBool::new(false);
    let mut watcher = IndexWatcher::start(&root, None, &cancel).unwrap();
    settle(&mut watcher);
    fs::write(root.join("old/sub/file"), [0; 8193]).unwrap();
    settle(&mut watcher);
    fs::rename(root.join("old"), root.join("new")).unwrap();
    fs::write(root.join("new/sub/file"), [0; 129]).unwrap();
    settle(&mut watcher);
    fs::create_dir_all(root.join("arrived/deep")).unwrap();
    fs::write(root.join("arrived/deep/added"), [0; 73]).unwrap();
    fs::hard_link(root.join("new/sub/file"), root.join("alias")).unwrap();
    settle(&mut watcher);
    fs::remove_dir_all(root.join("new")).unwrap();
    settle(&mut watcher);
    let database = temp.path().join("checkpoint");
    watcher.index.save(&database).unwrap();
    drop(watcher);
    fs::write(root.join("offline"), [0; 3001]).unwrap();
    let saved = PersistentIndex::load(&database).unwrap();
    let mut watcher = IndexWatcher::start(&root, Some(saved), &cancel).unwrap();
    settle(&mut watcher);

    // Exercise the public recovery path with a reset at the exact last cursor.
    let events = Arc::new(Mutex::new(Events::default()));
    send(&events, vec![Change::Reset("injected loss")]);
    watcher.source = Box::new(Mock {
        cursor: watcher.index.cursor.unwrap(),
        events,
    });
    let recovered = watcher.poll(&cancel).unwrap();
    assert!(recovered.rebuilt);
    settle(&mut watcher);
    watcher.last_rebuild -= Duration::from_secs(901);
    assert!(watcher.poll(&cancel).unwrap().rebuilt);
    settle(&mut watcher);
}

#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
fn settle(watcher: &mut IndexWatcher) {
    let cancel = AtomicBool::new(false);
    let expected = PersistentIndex::scan_snapshot(&watcher.root, &cancel).unwrap();
    let expected_total = expected.total(expected.root).unwrap().0;
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let update = watcher.poll(&cancel).unwrap();
        if update.caught_up
            && watcher.index.total(watcher.index.root).unwrap().0 == expected_total
            && watcher.index.directory_count() == expected.directory_count()
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "native replay failed to reach baseline: actual {:?}, expected {:?}",
            watcher.index.total(watcher.index.root),
            expected_total
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_baseline(watcher);
}

#[test]
fn arrived_directory_retires_a_deleted_file_identity_before_subtree_removal() {
    for reused_path in ["arrived", "arrived/inner"] {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join("removed/child")).unwrap();
        fs::write(temp.path().join("removed/child/f"), [0; 7]).unwrap();
        let (mut watcher, events) = start(temp.path());
        let root = watcher.index.root();
        let removed = observe(&temp.path().join("removed")).unwrap().id;
        let child = observe(&temp.path().join("removed/child")).unwrap().id;
        fs::create_dir_all(temp.path().join("arrived/inner")).unwrap();
        fs::write(temp.path().join("arrived/inner/g"), [0; 103]).unwrap();
        let arrived = observe(&temp.path().join(reused_path)).unwrap();
        // Model inode recycling deterministically, independent of the host allocator.
        let stale = LinkKey {
            parent: child,
            name: "f".into(),
        };
        watcher.index.remove(&stale, None).unwrap();
        watcher
            .index
            .upsert(
                stale,
                ObservedEntry {
                    id: arrived.id,
                    kind: EntryKind::File,
                    logical: 7,
                    physical: 7,
                },
            )
            .unwrap();
        fs::remove_dir_all(temp.path().join("removed")).unwrap();
        send(
            &events,
            vec![change(root, "removed"), change(root, "arrived")],
        );
        let result = watcher.drain(&AtomicBool::new(false)).unwrap();
        assert!(result.caught_up);
        assert_eq!(result.stats.directories_read, 2);
        let released = events.lock().unwrap().removed.clone();
        assert!(released.contains(&removed) && released.contains(&child));
        assert!(!released.contains(&root));
        assert_eq!(watcher.index.total(root).unwrap().1, Freshness::Observed);
        assert_baseline(&watcher);
    }
}
#[test]
fn reused_file_identity_keeps_all_links_when_any_link_is_still_live() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("z-live"), [0; 17]).unwrap();
    let (mut watcher, _) = start(temp.path());
    let root = watcher.index.root();
    let file = observe(&temp.path().join("z-live")).unwrap();
    let absent = LinkKey {
        parent: root,
        name: "a-absent".into(),
    };
    watcher.index.upsert(absent.clone(), file).unwrap();
    let before = watcher.index.total(root).unwrap();
    let error = watcher
        .index
        .apply_observed(
            LinkKey {
                parent: root,
                name: "arrived".into(),
            },
            Some(ObservedEntry {
                kind: EntryKind::Directory,
                ..file
            }),
            temp.path(),
            false,
            watcher.source.as_mut(),
            &AtomicBool::new(false),
            &mut UpdateStats::default(),
        )
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert_eq!(watcher.index.links(file.id).len(), 2);
    assert_eq!(watcher.index.lookup(root, &absent.name), Some(file.id));
    assert_eq!(watcher.index.total(root).unwrap(), before);
    assert_eq!(watcher.index.lookup(root, OsStr::new("arrived")), None);
}

#[test]
fn reused_file_identity_with_a_replaced_old_name_requires_reconciliation() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("replacement"), [0; 29]).unwrap();
    let (mut watcher, _) = start(temp.path());
    let root = watcher.index.root();
    fs::create_dir(temp.path().join("arrived")).unwrap();
    let directory = observe(&temp.path().join("arrived")).unwrap();
    let old = LinkKey {
        parent: root,
        name: "replacement".into(),
    };
    watcher.index.remove(&old, None).unwrap();
    watcher
        .index
        .upsert(
            old.clone(),
            ObservedEntry {
                id: directory.id,
                kind: EntryKind::File,
                logical: 7,
                physical: 7,
            },
        )
        .unwrap();
    let before = watcher.index.total(root).unwrap();
    let error = watcher
        .index
        .apply_observed(
            LinkKey {
                parent: root,
                name: "arrived".into(),
            },
            Some(directory),
            temp.path(),
            false,
            watcher.source.as_mut(),
            &AtomicBool::new(false),
            &mut UpdateStats::default(),
        )
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert_eq!(watcher.index.lookup(root, &old.name), Some(directory.id));
    assert_eq!(watcher.index.total(root).unwrap(), before);
    assert_eq!(watcher.index.lookup(root, OsStr::new("arrived")), None);
}

#[cfg(target_os = "linux")]
#[test]
fn native_arrived_subtree_with_recycled_directory_ids_is_observed_and_watched() {
    let temp = tempfile::tempdir().unwrap();
    let root_path = temp.path().join("root");
    fs::create_dir_all(root_path.join("removed/child")).unwrap();
    fs::write(root_path.join("removed/child/stale"), [0; 17]).unwrap();
    let outside = temp.path().join("outside");
    fs::create_dir_all(outside.join("inner")).unwrap();
    fs::write(outside.join("inner/file"), [0; 103]).unwrap();
    let cancel = AtomicBool::new(false);
    let mut watcher = IndexWatcher::start(&root_path, None, &cancel).unwrap();
    watcher.drain(&cancel).unwrap();
    let root = watcher.index.root();
    let arrived = observe(&outside).unwrap();
    let inner = observe(&outside.join("inner")).unwrap();
    let stale = observe(&root_path.join("removed/child/stale")).unwrap();
    // Model recycled directory IDs without depending on the filesystem's allocator.
    // The native queue still supplies the real outside-to-inside move event.
    let removed = LinkKey {
        parent: root,
        name: "removed".into(),
    };
    watcher.index.remove(&removed, None).unwrap();
    watcher.index.upsert(removed, arrived).unwrap();
    watcher
        .index
        .upsert(
            LinkKey {
                parent: arrived.id,
                name: "child".into(),
            },
            inner,
        )
        .unwrap();
    watcher
        .index
        .upsert(
            LinkKey {
                parent: inner.id,
                name: "stale".into(),
            },
            stale,
        )
        .unwrap();
    fs::remove_dir_all(root_path.join("removed")).unwrap();
    fs::rename(outside, root_path.join("arrived")).unwrap();
    let update = watcher.drain(&cancel).unwrap();
    assert!(update.caught_up);
    assert_eq!(update.stats.directories_read, 2);
    assert_baseline(&watcher);
    let backend = watcher.source.cursor().kind;
    eprintln!("native recycled directory fixture backend={backend:?}");
    fs::rename(root_path.join("arrived"), root_path.join("renamed")).unwrap();
    let moved = watcher.drain(&cancel).unwrap();
    if backend == JournalKind::Inotify {
        assert_eq!(moved.stats.directories_read, 0);
    }
    assert_eq!(
        watcher.index.lookup(root, OsStr::new("renamed")),
        Some(arrived.id)
    );
    assert_baseline(&watcher);
    // A later descendant write proves the new incarnation stays watched after rename.
    fs::write(root_path.join("renamed/inner/file"), [1; 8193]).unwrap();
    assert!(watcher.drain(&cancel).unwrap().caught_up);
    assert_baseline(&watcher);
}
