use std::fs;

use super::*;

fn id(path: &Path) -> EntryId {
    let stat = fs::symlink_metadata(path).unwrap();
    EntryId {
        volume: stat.dev(),
        object: u128::from(stat.ino()),
    }
}
fn record(wd: i32, mask: u32, name: &[u8]) -> Vec<u8> {
    let mut value = Vec::new();
    value.extend_from_slice(&wd.to_ne_bytes());
    value.extend_from_slice(&mask.to_ne_bytes());
    value.extend_from_slice(&0u32.to_ne_bytes());
    value.extend_from_slice(&(name.len() as u32).to_ne_bytes());
    value.extend_from_slice(name);
    value
}
fn drain(source: &mut LinuxJournal) -> Vec<Change> {
    let mut changes = Vec::new();
    for _ in 0..100 {
        let batch = source.poll().unwrap();
        assert!(
            !batch
                .changes
                .iter()
                .any(|change| matches!(change, Change::Reset(_))),
            "unexpected reset"
        );
        assert_eq!(
            batch.next.position - batch.from.position,
            u64::from(!batch.changes.is_empty())
        );
        changes.extend(batch.changes);
        if batch.caught_up {
            return changes;
        }
    }
    panic!("native event stream failed to drain");
}
fn touched(changes: &[Change], parent: EntryId, name: &str) -> bool {
    changes.iter().any(|change| matches!(change, Change::Entry { key, .. } if key.parent == parent && key.name == name))
}

#[test]
fn inotify_rejects_bad_frames_and_reports_gaps() {
    let root = EntryId {
        volume: 1,
        object: 2,
    };
    let mut source = Inotify::open(root).unwrap();
    source.watches.insert(1, root);
    source.reverse.insert(root, 1);
    for frame in [
        vec![0; 15],
        record(1, libc::IN_CREATE, b"not-terminated"),
        record(1, libc::IN_CREATE, b"../bad\0"),
    ] {
        assert!(source.decode(&frame, &mut Vec::new()).is_err());
    }
    for mask in [
        libc::IN_Q_OVERFLOW,
        libc::IN_UNMOUNT,
        libc::IN_IGNORED,
        libc::IN_MOVE_SELF,
        libc::IN_DELETE_SELF,
    ] {
        let mut changes = Vec::new();
        source.decode(&record(1, mask, b""), &mut changes).unwrap();
        assert!(changes
            .iter()
            .any(|change| matches!(change, Change::Reset(_))));
    }
}

#[test]
fn inotify_expected_watch_removal_and_child_move_are_not_gaps() {
    let root = EntryId {
        volume: 1,
        object: 2,
    };
    let child = EntryId {
        volume: 1,
        object: 3,
    };
    let mut source = Inotify::open(root).unwrap();
    source.watches.insert(2, child);
    source.reverse.insert(child, 2);
    let mut changes = Vec::new();
    source
        .decode(&record(2, libc::IN_MOVE_SELF, b""), &mut changes)
        .unwrap();
    source
        .decode(&record(2, libc::IN_DELETE_SELF, b""), &mut changes)
        .unwrap();
    source
        .decode(&record(2, libc::IN_IGNORED, b""), &mut changes)
        .unwrap();
    assert!(changes.is_empty());
    assert!(!source.watches.contains_key(&2));
}

// /tmp intentionally avoids DrvFS: Windows writes do not promise Linux notifications.
#[test]
fn native_inotify_create_write_rename_delete_and_subtree() {
    let temp = tempfile::tempdir_in("/tmp").unwrap();
    let root = id(temp.path());
    let (mut source, resumed) =
        LinuxJournal::open_with_backend(temp.path(), root, "inotify").unwrap();
    assert!(!resumed);
    assert_eq!(source.cursor().kind, JournalKind::Inotify);
    assert!(source.poll().unwrap().caught_up);
    fs::write(temp.path().join("one"), b"one").unwrap();
    assert!(touched(&drain(&mut source), root, "one"));
    fs::write(temp.path().join("one"), b"more bytes").unwrap();
    assert!(touched(&drain(&mut source), root, "one"));
    fs::rename(temp.path().join("one"), temp.path().join("two")).unwrap();
    let changes = drain(&mut source);
    assert!(touched(&changes, root, "one") && touched(&changes, root, "two"));
    fs::remove_file(temp.path().join("two")).unwrap();
    assert!(touched(&drain(&mut source), root, "two"));
    let sub = temp.path().join("sub");
    fs::create_dir(&sub).unwrap();
    assert!(touched(&drain(&mut source), root, "sub"));
    let child = id(&sub);
    source.register_directory(&sub, child).unwrap();
    fs::write(sub.join("inside"), b"x").unwrap();
    assert!(touched(&drain(&mut source), child, "inside"));
    let moved = temp.path().join("moved");
    fs::rename(&sub, &moved).unwrap();
    let changes = drain(&mut source);
    assert!(touched(&changes, root, "sub") && touched(&changes, root, "moved"));
    fs::write(moved.join("inside"), b"xx").unwrap();
    assert!(touched(&drain(&mut source), child, "inside"));
    source.unregister_directory(child).unwrap();
    drain(&mut source);
    fs::write(moved.join("inside"), b"xxx").unwrap();
    assert!(drain(&mut source).is_empty());
    let raw_name = OsString::from_vec(vec![b'x', 0xff]);
    fs::write(temp.path().join(&raw_name), b"raw bytes").unwrap();
    assert!(drain(&mut source)
        .iter()
        .any(|event| matches!(event, Change::Entry { key, .. } if key.name == raw_name)));
    let previous = source.cursor();
    let (other, resumed) = LinuxJournal::open(temp.path(), root, Some(previous)).unwrap();
    assert!(!resumed);
    assert_ne!(other.cursor().epoch, previous.epoch);
}

#[test]
fn native_inotify_root_move_and_registration_identity() {
    let temp = tempfile::tempdir_in("/tmp").unwrap();
    let path = temp.path().join("root");
    fs::create_dir(&path).unwrap();
    let root = id(&path);
    let (mut source, _) = LinuxJournal::open_with_backend(&path, root, "inotify").unwrap();
    assert!(source
        .register_directory(
            &path,
            EntryId {
                object: root.object + 1,
                ..root
            }
        )
        .is_err());
    fs::rename(&path, temp.path().join("moved")).unwrap();
    assert!(source
        .poll()
        .unwrap()
        .changes
        .iter()
        .any(|event| matches!(event, Change::Reset(_))));
}

fn fan_record(mask: u64, info: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&((24 + info.len()) as u32).to_ne_bytes());
    bytes.extend_from_slice(&[3, 0]);
    bytes.extend_from_slice(&24u16.to_ne_bytes());
    bytes.extend_from_slice(&mask.to_ne_bytes());
    bytes.extend_from_slice(&(-1i32).to_ne_bytes());
    bytes.extend_from_slice(&0i32.to_ne_bytes());
    bytes.extend_from_slice(info);
    bytes
}
#[test]
fn fanotify_checked_frames_overflow_and_parent_handle() {
    let root = EntryId {
        volume: 1,
        object: 2,
    };
    // Parser owns a harmless eventfd; it never performs fanotify operations on it.
    let fd = owned_fd(unsafe { libc::eventfd(0, libc::EFD_CLOEXEC) }).unwrap();
    let mut source = Fanotify {
        fd,
        root,
        handles: HashMap::new(),
        reverse: HashMap::new(),
    };
    source.handles.insert(
        HandleKey {
            fsid: [0; 8],
            kind: 1,
            bytes: vec![42],
        },
        root,
    );
    let mut info = vec![2, 0, 0, 0];
    info.extend_from_slice(&[0; 8]);
    info.extend_from_slice(&1u32.to_ne_bytes());
    info.extend_from_slice(&1i32.to_ne_bytes());
    info.extend_from_slice(&[42, b'x', 0]);
    let len = info.len() as u16;
    info[2..4].copy_from_slice(&len.to_ne_bytes());
    let mut changes = Vec::new();
    source
        .decode(&fan_record(0x100, &info), &mut changes)
        .unwrap();
    assert!(touched(&changes, root, "x"));
    info[20] = 43;
    source
        .decode(&fan_record(0x100, &info), &mut Vec::new())
        .unwrap();
    let mut overflow = Vec::new();
    source
        .decode(&fan_record(0x4000, &[]), &mut overflow)
        .unwrap();
    assert!(matches!(overflow[0], Change::Reset(_)));
    for frame in [
        vec![0; 23],
        fan_record(0x100, &[]),
        fan_record(0x100, &[2, 0, 255, 255]),
        fan_record(0x100, &info[..22]),
    ] {
        assert!(source.decode(&frame, &mut Vec::new()).is_err());
    }
}

#[test]
fn native_fanotify_when_owned_fixture_is_requested() {
    let Some(root) = std::env::var_os("HYPERDU_TEST_FANOTIFY_ROOT") else {
        eprintln!("NOT RUN: native fanotify requires HYPERDU_TEST_FANOTIFY_ROOT on an owned Linux filesystem with CAP_SYS_ADMIN");
        return;
    };
    let temp = tempfile::tempdir_in(root).unwrap();
    let root = id(temp.path());
    let (mut source, resumed) = LinuxJournal::open_with_backend(temp.path(), root, "fanotify")
        .expect("explicit fanotify fixture requires native fanotify, no fallback");
    assert!(!resumed);
    assert_eq!(source.cursor().kind, JournalKind::Fanotify);
    drain(&mut source);
    fs::write(temp.path().join("witness"), b"fanotify").unwrap();
    assert!(touched(&drain(&mut source), root, "witness"));
    fs::rename(temp.path().join("witness"), temp.path().join("renamed")).unwrap();
    let changes = drain(&mut source);
    assert!(touched(&changes, root, "witness") && touched(&changes, root, "renamed"));
    fs::remove_file(temp.path().join("renamed")).unwrap();
    assert!(touched(&drain(&mut source), root, "renamed"));
    let sub = temp.path().join("child");
    fs::create_dir(&sub).unwrap();
    assert!(touched(&drain(&mut source), root, "child"));
    let child = id(&sub);
    source.register_directory(&sub, child).unwrap();
    fs::write(sub.join("nested"), b"fanotify only, no inotify child watch").unwrap();
    assert!(touched(&drain(&mut source), child, "nested"));
    let moved = temp.path().join("moved");
    fs::rename(&sub, &moved).unwrap();
    drain(&mut source);
    fs::write(moved.join("nested"), b"identity survived rename").unwrap();
    assert!(touched(&drain(&mut source), child, "nested"));
    source.unregister_directory(child).unwrap();
    fs::write(moved.join("nested"), b"outside registry").unwrap();
    assert!(!drain(&mut source)
        .iter()
        .any(|event| matches!(event, Change::Entry { key, .. } if key.parent == child)));
}

#[test]
fn fanotify_dot_metadata_and_descriptor_error_cleanup() {
    let root = EntryId {
        volume: 1,
        object: 2,
    };
    // SAFETY: eventfd takes scalar arguments and returns a fresh owned descriptor.
    let fd = owned_fd(unsafe { libc::eventfd(0, libc::EFD_CLOEXEC) }).unwrap();
    let mut source = Fanotify {
        fd,
        root,
        handles: HashMap::new(),
        reverse: HashMap::new(),
    };
    source.handles.insert(
        HandleKey {
            fsid: [0; 8],
            kind: 1,
            bytes: vec![42],
        },
        root,
    );
    let mut info = vec![2, 0, 23, 0];
    info.extend_from_slice(&[0; 8]);
    info.extend_from_slice(&1u32.to_ne_bytes());
    info.extend_from_slice(&1i32.to_ne_bytes());
    info.extend_from_slice(&[42, b'.', 0]);
    let mut changes = Vec::new();
    source
        .decode(&fan_record(0x4000_0004, &info), &mut changes)
        .unwrap();
    assert!(changes.is_empty());
    source
        .decode(&fan_record(0x4000_0800, &info), &mut changes)
        .unwrap();
    assert!(matches!(changes[0], Change::Reset(_)));
    let mut stream = fan_record(0x100, &[]); // invalid first event must not leak later fd
                                             // SAFETY: duplicate a live descriptor; the decoder now exclusively owns the duplicate.
    let duplicate = unsafe { libc::fcntl(source.fd.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 256) };
    assert!(duplicate >= 0);
    let mut second = fan_record(0x100, &[]);
    second[16..20].copy_from_slice(&duplicate.to_ne_bytes());
    stream.extend(second);
    assert!(source.decode(&stream, &mut Vec::new()).is_err());
    // SAFETY: F_GETFD is a read-only validity check on the previously owned descriptor.
    assert_eq!(unsafe { libc::fcntl(duplicate, libc::F_GETFD) }, -1);
    assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::EBADF));
}

#[test]
fn inotify_registration_refuses_watch_reuse_and_symlinks() {
    let temp = tempfile::tempdir_in("/tmp").unwrap();
    let root = id(temp.path());
    let mut source = Inotify::open(root).unwrap();
    source.register(temp.path(), root).unwrap();
    let wd = source.reverse[&root];
    source.retiring.insert(wd);
    assert!(source.register(temp.path(), root).is_err());
    let link = temp.path().join("link");
    std::os::unix::fs::symlink(temp.path(), &link).unwrap();
    assert!(source.register(&link, root).is_err());
}

#[test]
fn native_unsupported_fanotify_falls_back_only_in_auto_mode() {
    // procfs has no exportable FID journal; registration is read-only and does no scan.
    let path = Path::new("/proc");
    let root = id(path);
    assert!(LinuxJournal::open_with_backend(path, root, "fanotify").is_err());
    let (source, resumed) = LinuxJournal::open_with_backend(path, root, "auto").unwrap();
    assert_eq!(source.cursor().kind, JournalKind::Inotify);
    assert!(!resumed);
}

#[test]
fn directory_creation_forces_subtree_observation_but_rename_retains_children() {
    let root = EntryId {
        volume: 1,
        object: 2,
    };
    let mut source = Inotify::open(root).unwrap();
    source.watches.insert(1, root);
    source.reverse.insert(root, 1);
    for (mask, recursive) in [(libc::IN_CREATE, true), (libc::IN_MOVED_TO, false)] {
        let mut changes = Vec::new();
        source
            .decode(&record(1, mask | libc::IN_ISDIR, b"dir\0"), &mut changes)
            .unwrap();
        assert!(
            matches!(&changes[0], Change::Entry { subtree, .. } if *subtree == recursive),
            "directory birth must not be confused with an existing inode's move"
        );
    }
    // SAFETY: eventfd returns a new harmless descriptor solely for parser ownership.
    let fd = owned_fd(unsafe { libc::eventfd(0, libc::EFD_CLOEXEC) }).unwrap();
    let mut source = Fanotify {
        fd,
        root,
        handles: HashMap::new(),
        reverse: HashMap::new(),
    };
    source.handles.insert(
        HandleKey {
            fsid: [0; 8],
            kind: 1,
            bytes: vec![42],
        },
        root,
    );
    let mut info = vec![2, 0, 23, 0];
    info.extend_from_slice(&[0; 8]);
    info.extend_from_slice(&1u32.to_ne_bytes());
    info.extend_from_slice(&1i32.to_ne_bytes());
    info.extend_from_slice(&[42, b'x', 0]);
    for (mask, recursive) in [(0x100, true), (0x80, false)] {
        let mut changes = Vec::new();
        source
            .decode(&fan_record(mask | 0x4000_0000, &info), &mut changes)
            .unwrap();
        assert!(matches!(&changes[0], Change::Entry { subtree, .. } if *subtree == recursive));
    }
}
