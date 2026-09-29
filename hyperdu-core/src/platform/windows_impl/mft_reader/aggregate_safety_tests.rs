mod aggregate_safety {
    use super::*;

    fn checked(entries: &[Entry]) -> Option<crate::StatMap> {
        try_to_stat_map(entries, "C:\\", true)
    }

    #[test]
    fn missing_and_non_directory_parents_decline_instead_of_losing_bytes() {
        assert!(checked(&[entry(16, 999, "orphan", false)]).is_none());
        assert!(checked(&[
            entry(16, ROOT_RECORD, "file-parent", false),
            entry(17, 16, "file", false),
        ]).is_none());
    }

    #[test]
    fn cycles_duplicate_ids_paths_and_invalid_components_decline() {
        assert!(checked(&[
            entry(16, 17, "a", true), entry(17, 16, "b", true),
        ]).is_none());
        assert!(checked(&[
            entry(16, ROOT_RECORD, "a", true), entry(16, ROOT_RECORD, "b", true),
        ]).is_none());
        assert!(checked(&[
            entry(16, ROOT_RECORD, "same", true), entry(17, ROOT_RECORD, "same", true),
        ]).is_none());
        for name in ["", ".", "..", "a/b", "a\\b", "nul\0tail"] {
            assert!(checked(&[entry(16, ROOT_RECORD, name, true)]).is_none());
        }
    }

    #[test]
    fn known_ntfs_metadata_is_not_confused_with_an_unresolved_user_parent() {
        let map = checked(&[
            entry(16, 11, "metadata-directory", true),
            entry(17, 16, "metadata-file", false),
            entry(18, ROOT_RECORD, "visible", false),
        ]).unwrap();
        assert_eq!(map.len(), 1);
        assert_eq!(map[&PathBuf::from("C:\\")].files, 1);
        assert_eq!(map[&PathBuf::from("C:\\")].logical, 19);
    }

    #[test]
    fn valid_deep_tree_is_complete_in_both_record_orders() {
        let mut entries = Vec::new();
        let mut parent = ROOT_RECORD;
        for record in 16..316 {
            entries.push(entry(record, parent, "d", true));
            parent = record;
        }
        entries.push(entry(400, parent, "file", false));
        let forward = values(checked(&entries).unwrap());
        assert_eq!(forward.len(), 301);
        assert_eq!(forward.values().map(|v| v.0).sum::<u64>(), 1);
        entries.reverse();
        assert_eq!(forward, values(checked(&entries).unwrap()));
    }

    #[test]
    fn exact_unnamed_totals_and_overflow_rejection() {
        let mut files = vec![
            entry(16, ROOT_RECORD, "a", false),
            entry(17, ROOT_RECORD, "b", false),
        ];
        let map = checked(&files).unwrap();
        assert_eq!(map[&PathBuf::from("C:\\")].logical, 35);
        assert_eq!(map[&PathBuf::from("C:\\")].physical, 33 * 4096);
        files[0].sizes.real_size = u64::MAX;
        assert!(checked(&files).is_none());
    }
}
