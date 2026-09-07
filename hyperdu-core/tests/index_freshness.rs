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
