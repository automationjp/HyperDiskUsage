#[cfg(unix)]
#[test]
#[ignore = "Known discrepancy in subtree aggregation on some filesystems; tracked for follow-up."]
fn hardlink_dedupe_unix() {
    use hyperdu_core::{scan_directory, Options, Stat};
    use std::fs::{create_dir_all, hard_link, File};
    use std::io::Write;
    use std::path::PathBuf;

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("d");
    create_dir_all(&root).unwrap();
    let f1 = root.join("a.bin");
    let mut fh = File::create(&f1).unwrap();
    fh.write_all(&vec![1u8; 8192]).unwrap();
    let f2 = root.join("b.bin");
    hard_link(&f1, &f2).unwrap();

    // dedupe (GNU du 互換)
    let mut opt = Options::default();
    opt.compute_physical = false;
    opt.count_hardlinks = false;
    opt.inode_cache = Some(std::sync::Arc::new(dashmap::DashMap::with_capacity(16)));
    let map = scan_directory(&root, &opt).unwrap();
    let (files_sum, logical_sum): (u64, u64) = map
        .iter()
        .filter(|(p, _)| p.starts_with(&root))
        .map(|(_, s)| (s.files, s.logical))
        .fold((0, 0), |acc, x| (acc.0 + x.0, acc.1 + x.1));
    // dedupe: one logical file counted across the subtree
    assert_eq!(files_sum, 1, "dedupe should count one logical file");
    assert!(logical_sum >= 8192 && logical_sum < 16384);

    // non-dedupe（ハードリンクを別物としてカウント）
    let mut opt2 = opt.clone();
    opt2.count_hardlinks = true;
    opt2.inode_cache = None;
    let map2 = scan_directory(&root, &opt2).unwrap();
    let (files_sum2, logical_sum2): (u64, u64) = map2
        .iter()
        .filter(|(p, _)| p.starts_with(&root))
        .map(|(_, s)| (s.files, s.logical))
        .fold((0, 0), |acc, x| (acc.0 + x.0, acc.1 + x.1));
    assert_eq!(files_sum2, 2);
    assert!(logical_sum2 >= 16384);
}
