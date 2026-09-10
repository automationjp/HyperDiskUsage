use super::*;
use crate::index::journal::JournalKind;

fn id(object: u128) -> EntryId {
    EntryId { volume: 7, object }
}
fn key(parent: u128, name: &str) -> LinkKey {
    LinkKey {
        parent: id(parent),
        name: name.into(),
    }
}
fn dir(index: &mut PersistentIndex, parent: u128, name: &str, object: u128) {
    index
        .upsert(
            key(parent, name),
            ObservedEntry {
                id: id(object),
                kind: EntryKind::Directory,
                logical: 0,
                physical: 0,
            },
        )
        .unwrap();
}
fn file(index: &mut PersistentIndex, parent: u128, name: &str, object: u128, bytes: u64) {
    index
        .upsert(
            key(parent, name),
            ObservedEntry {
                id: id(object),
                kind: EntryKind::File,
                logical: bytes,
                physical: bytes * 2,
            },
        )
        .unwrap();
}
fn total(index: &PersistentIndex, object: u128) -> Totals {
    index.total(id(object)).unwrap().0
}

#[test]
fn hardlink_owner_promotes_and_metadata_delta_reaches_only_ancestors() {
    let mut index = PersistentIndex::new(id(1));
    dir(&mut index, 1, "a", 2);
    dir(&mut index, 1, "b", 3);
    file(&mut index, 3, "z", 10, 12);
    file(&mut index, 2, "x", 10, 12);
    assert_eq!(
        total(&index, 1),
        Totals {
            logical: 12,
            physical: 24,
            files: 1
        }
    );
    assert_eq!(total(&index, 2).logical, 12);
    assert_eq!(total(&index, 3).logical, 0);
    file(&mut index, 3, "z", 10, 20);
    assert_eq!(total(&index, 2).logical, 20);
    index.remove(&key(2, "x"), Some(id(10))).unwrap();
    assert_eq!(total(&index, 2).logical, 0);
    assert_eq!(total(&index, 3).logical, 20);
    assert_eq!(index.file_count(), 1);
    index.remove(&key(3, "z"), Some(id(10))).unwrap();
    assert_eq!(total(&index, 1), Totals::default());
    assert_eq!(index.file_count(), 0);
}

#[test]
fn directory_rename_retains_children_and_delete_preserves_external_hardlink() {
    let mut index = PersistentIndex::new(id(1));
    dir(&mut index, 1, "a", 2);
    dir(&mut index, 1, "b", 3);
    dir(&mut index, 2, "deep", 4);
    file(&mut index, 4, "file", 10, 100);
    file(&mut index, 3, "link", 10, 100);
    file(&mut index, 4, "other", 11, 40);
    dir(&mut index, 3, "moved", 2);
    assert_eq!(
        index.relative_path(id(4)),
        Some(PathBuf::from("b").join("moved").join("deep"))
    );
    assert_eq!(index.lookup(id(1), OsStr::new("a")), None);
    assert_eq!(total(&index, 1).logical, 140);
    assert_eq!(total(&index, 3).logical, 140);
    index.remove(&key(3, "moved"), Some(id(2))).unwrap();
    assert_eq!(
        total(&index, 1),
        Totals {
            logical: 100,
            physical: 200,
            files: 1
        }
    );
    assert_eq!(index.directory_count(), 2);
    assert_eq!(index.links(id(10)), vec![key(3, "link")]);
}

#[test]
fn stale_event_cannot_remove_replacement_and_invalid_moves_fail() {
    let mut index = PersistentIndex::new(id(1));
    dir(&mut index, 1, "a", 2);
    dir(&mut index, 2, "b", 3);
    file(&mut index, 1, "file", 10, 5);
    file(&mut index, 1, "file", 11, 9);
    assert!(index.remove(&key(1, "file"), Some(id(10))).is_err());
    assert_eq!(total(&index, 1).logical, 9);
    assert!(index
        .upsert(
            key(3, "cycle"),
            ObservedEntry {
                id: id(2),
                kind: EntryKind::Directory,
                logical: 0,
                physical: 0,
            }
        )
        .is_err());
    assert_eq!(
        index.relative_path(id(3)),
        Some(PathBuf::from("a").join("b"))
    );
    for name in ["", ".", "..", "a/b", "a/"] {
        assert!(validate_name(OsStr::new(name)).is_err(), "{name:?}");
    }
}

#[test]
fn upsert_rejects_foreign_volume_without_publishing() {
    let mut index = PersistentIndex::new(id(1));
    let foreign = index.upsert(
        key(1, "foreign"),
        ObservedEntry {
            id: EntryId {
                volume: 8,
                object: 10,
            },
            kind: EntryKind::File,
            logical: 4,
            physical: 8,
        },
    );
    assert!(foreign.is_err());
    assert_eq!(index.lookup(id(1), OsStr::new("foreign")), None);
    assert_eq!(index.file_count(), 0);
    assert_eq!(index.directory_count(), 1);
}
#[test]
fn overflow_never_saturates_or_claims_freshness() {
    let mut index = PersistentIndex::new(id(1));
    index
        .upsert(
            key(1, "max"),
            ObservedEntry {
                id: id(10),
                kind: EntryKind::File,
                logical: u64::MAX,
                physical: 0,
            },
        )
        .unwrap();
    let result = index.upsert(
        key(1, "overflow"),
        ObservedEntry {
            id: id(11),
            kind: EntryKind::File,
            logical: 1,
            physical: 0,
        },
    );
    assert!(result.is_err());
    let temp = tempfile::tempdir().unwrap();
    assert!(index.save(&temp.path().join("failed")).is_err());
    assert!(!temp.path().join("failed").exists());
    assert_eq!(
        index.total(id(1)).unwrap(),
        (
            Totals {
                logical: u64::MAX,
                physical: 0,
                files: 1
            },
            Freshness::Stale
        )
    );
}

#[test]
fn atomic_snapshot_roundtrip_preserves_ids_names_cursor_but_loads_stale() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("index");
    let mut index = PersistentIndex::new(id(1));
    dir(&mut index, 1, "dir", 2);
    file(&mut index, 2, "one", 1_u128 << 100, 1024);
    file(&mut index, 1, "alias", 1_u128 << 100, 1024);
    #[cfg(unix)]
    let raw = {
        use std::os::unix::ffi::OsStringExt;
        OsString::from_vec(vec![0x66, 0xff])
    };
    #[cfg(windows)]
    let raw = {
        use std::os::windows::ffi::OsStringExt;
        OsString::from_wide(&[0x66, 0xd800])
    };
    #[cfg(not(any(unix, windows)))]
    let raw = OsString::from("raw");
    index
        .upsert(
            LinkKey {
                parent: id(2),
                name: raw.clone(),
            },
            ObservedEntry {
                id: id(11),
                kind: EntryKind::File,
                logical: 3,
                physical: 4096,
            },
        )
        .unwrap();
    let cursor = JournalCursor {
        kind: JournalKind::Usn,
        volume: 7,
        epoch: 123,
        position: 500,
    };
    index.mark_current(cursor).unwrap();
    index.save(&path).unwrap();
    // Atomic replacement of an existing snapshot is also required.
    index.save(&path).unwrap();
    let loaded = PersistentIndex::load(&path).unwrap();
    assert_eq!(
        loaded.total(id(1)),
        Some((total(&index, 1), Freshness::Stale))
    );
    assert_eq!(loaded.cursor(), Some(cursor));
    assert_eq!(loaded.lookup(id(2), &raw), Some(id(11)));
    assert_eq!(loaded.links(id(1_u128 << 100)).len(), 2);
    let bytes = std::fs::read(&path).unwrap();
    for cut in [0, 7, 20, bytes.len() - 1] {
        std::fs::write(&path, &bytes[..cut]).unwrap();
        assert!(PersistentIndex::load(&path).is_err());
    }
    let mut corrupt = bytes;
    corrupt[16] ^= 1;
    std::fs::write(&path, corrupt).unwrap();
    assert!(PersistentIndex::load(&path).is_err());
}
#[test]
fn observed_file_update_reads_no_directory_and_directory_rename_keeps_descendants() {
    use std::sync::atomic::AtomicBool;

    use crate::index::journal::{Batch, NativeJournal};
    struct Journal;
    impl NativeJournal for Journal {
        fn cursor(&self) -> JournalCursor {
            panic!("not needed for observation test")
        }
        fn poll(&mut self) -> io::Result<Batch> {
            panic!("not needed for observation test")
        }
    }
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("root");
    std::fs::create_dir(&root).unwrap();
    std::fs::create_dir(root.join("nested")).unwrap();
    for n in 0..40 {
        std::fs::write(root.join("nested").join(format!("file{n}")), [1; 80]).unwrap();
    }
    std::fs::write(root.join("changed"), [1; 3]).unwrap();
    let cancel = AtomicBool::new(false);
    let mut index = PersistentIndex::scan_snapshot(&root, &cancel).unwrap();
    let root_id = index.root();
    std::fs::write(root.join("changed"), [2; 9000]).unwrap();
    let mut stats = UpdateStats::default();
    index
        .refresh_entry(
            LinkKey {
                parent: root_id,
                name: "changed".into(),
            },
            &root,
            false,
            &mut Journal,
            &cancel,
            &mut stats,
        )
        .unwrap();
    assert_eq!(stats.observed_entries, 1);
    assert_eq!(stats.directories_read, 0);
    let expected = PersistentIndex::scan_snapshot(&root, &cancel).unwrap();
    assert_eq!(index.total(root_id), expected.total(root_id));
    let nested = index.directory_at(Path::new("nested")).unwrap();
    std::fs::rename(root.join("nested"), root.join("renamed")).unwrap();
    index
        .refresh_entry(
            LinkKey {
                parent: root_id,
                name: "renamed".into(),
            },
            &root,
            false,
            &mut Journal,
            &cancel,
            &mut stats,
        )
        .unwrap();
    index
        .refresh_entry(
            LinkKey {
                parent: root_id,
                name: "nested".into(),
            },
            &root,
            false,
            &mut Journal,
            &cancel,
            &mut stats,
        )
        .unwrap();
    assert_eq!(stats.directories_read, 0);
    assert_eq!(index.directory_at(Path::new("renamed")), Some(nested));
    assert_eq!(index.file_count(), 41);
    let expected = PersistentIndex::scan_snapshot(&root, &cancel).unwrap();
    assert_eq!(index.total(root_id), expected.total(root_id));
    let stopped = AtomicBool::new(true);
    assert!(PersistentIndex::scan_snapshot(&root, &stopped).is_err());
}

#[cfg(unix)]
#[test]
fn baseline_excludes_symlink_and_deduplicates_hardlinks_without_losing_raw_names() {
    use std::{os::unix::fs::symlink, sync::atomic::AtomicBool};
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    std::fs::write(root.join("original"), [0; 5000]).unwrap();
    std::fs::hard_link(root.join("original"), root.join("alias")).unwrap();
    symlink("original", root.join("symlink")).unwrap();
    let index = PersistentIndex::scan_snapshot(root, &AtomicBool::new(false)).unwrap();
    let expected = observe(&root.join("original")).unwrap();
    assert_eq!(
        index.total(index.root()).unwrap(),
        (
            Totals {
                logical: 5000,
                physical: expected.physical,
                files: 1,
            },
            Freshness::Stale
        )
    );
}
