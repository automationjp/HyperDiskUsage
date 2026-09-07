use hyperdu_core::index::{Freshness, Index};

#[test]
fn loading_a_snapshot_does_not_claim_continuous_observation() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("index.bin");
    let mut index = Index::new();
    index.set_own((1, 1), 123, 1);
    index.save(&path).unwrap();
    let loaded = Index::load(&path).unwrap();
    assert_eq!(
        loaded.subtree_total((1, 1)),
        Some((123, 1, Freshness::Stale))
    );
}

#[test]
fn stale_child_invalidates_ancestor_totals_without_invalidating_siblings() {
    let mut index = Index::new();
    index.set_own((1, 1), 0, 0);
    index.set_own((1, 2), 10, 1);
    index.set_own((1, 3), 20, 1);
    index.link((1, 2), (1, 1));
    index.link((1, 3), (1, 1));
    index.mark_stale((1, 2));
    assert_eq!(index.get((1, 1)).unwrap().state, Freshness::Stale);
    assert_eq!(index.get((1, 3)).unwrap().state, Freshness::Fresh);
    index.set_own((1, 1), 5, 1);
    assert_eq!(index.get((1, 1)).unwrap().state, Freshness::Stale);
}

#[test]
fn corrupt_identity_tree_and_totals_are_refused() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("index.bin");
    let mut index = Index::new();
    index.set_own((1, 1), 123, 1);
    index.save(&path).unwrap();
    let original = std::fs::read(&path).unwrap();
    for offset in [20 + 32, 20 + 64] {
        let mut bytes = original.clone();
        bytes[offset] ^= 1;
        std::fs::write(&path, bytes).unwrap();
        assert!(Index::load(&path).is_err());
    }
    let mut duplicate = original.clone();
    duplicate[12..20].copy_from_slice(&2u64.to_le_bytes());
    duplicate.extend_from_slice(&original[20..]);
    std::fs::write(&path, duplicate).unwrap();
    assert!(Index::load(&path).is_err());
}

#[cfg(unix)]
#[test]
fn saving_does_not_follow_the_legacy_temporary_symlink() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("index.bin");
    let victim = temp.path().join("untouched");
    std::fs::write(&victim, b"keep me").unwrap();
    symlink(&victim, path.with_extension("tmp")).unwrap();
    let mut index = Index::new();
    index.set_own((1, 1), 1, 1);
    index.save(&path).unwrap();
    assert_eq!(std::fs::read(&victim).unwrap(), b"keep me");
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[cfg(target_os = "linux")]
#[test]
fn snapshot_reuses_sparse_and_hardlink_semantics_and_refuses_cancel() {
    use std::sync::{atomic::AtomicBool, Arc};
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    std::fs::write(root.join("data"), vec![1u8; 8192]).unwrap();
    std::fs::hard_link(root.join("data"), root.join("link")).unwrap();
    std::fs::File::create(root.join("sparse"))
        .unwrap()
        .set_len(1024 * 1024)
        .unwrap();
    let (key, index) = Index::scan_snapshot(root, Arc::new(AtomicBool::new(false))).unwrap();
    let exact = hyperdu_core::scan_directory(root, &Default::default()).unwrap();
    assert_eq!(
        index.subtree_total(key),
        Some((exact[root].physical, exact[root].files, Freshness::Stale))
    );
    assert!(Index::scan_snapshot(root, Arc::new(AtomicBool::new(true))).is_err());
}
