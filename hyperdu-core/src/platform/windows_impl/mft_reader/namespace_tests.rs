mod namespace_tests {
    use super::*;

    fn multiple_names(parents: &[u64]) -> Vec<u8> {
        let mut record = blank_record(1, parents.len() as u16);
        let mut offset = 64;
        for (index, parent) in parents.iter().enumerate() {
            offset = push_file_name(&mut record, offset, *parent, &format!("link-{index}"));
        }
        offset = push_nonresident_data(&mut record, offset, 4096, 71);
        set_used(&mut record, offset as u32);
        record
    }

    fn reader(record: Vec<u8>) -> MftReader<MemVolume> {
        MftReader::open(volume(with_metadata_records(vec![record], 5))).unwrap()
    }

    #[test]
    fn same_parent_hardlinks_are_counted_once_without_losing_link_identity() {
        let mut source = reader(multiple_names(&[ROOT_RECORD, ROOT_RECORD, ROOT_RECORD]));
        let entry = source.entry(16).unwrap();
        assert_eq!(entry.hard_link_count, 3);
        assert_eq!(entry.sizes.real_size, 71);
        assert!(source.is_complete());
    }

    #[test]
    fn dos_alias_is_part_of_record_link_count_but_not_an_extra_file() {
        let mut record = multiple_names(&[ROOT_RECORD, ROOT_RECORD]);
        let first_length = u32::from_le_bytes(record[68..72].try_into().unwrap()) as usize;
        record[64 + first_length + 24 + 65] = namespace::DOS;
        let mut source = reader(record);
        let entry = source.entry(16).unwrap();
        assert_eq!(entry.sizes.real_size, 71);
        assert_eq!(entry.name, "link-0");
        assert!(source.is_complete());
    }

    #[test]
    fn cross_parent_hardlinks_decline_instead_of_guessing_folder_attribution() {
        let mut source = reader(multiple_names(&[ROOT_RECORD, 17]));
        assert!(source.entry(16).is_none());
        assert!(!source.is_complete());
    }

    fn reparse_record(tag: u32, length: u16) -> Vec<u8> {
        let mut record = dir_record(ROOT_RECORD, "link");
        let pos = u32::from_le_bytes(record[24..28].try_into().unwrap()) as usize;
        record[pos..pos + 4].copy_from_slice(&0xC0u32.to_le_bytes());
        record[pos + 4..pos + 8].copy_from_slice(&32u32.to_le_bytes());
        record[pos + 16..pos + 20].copy_from_slice(&8u32.to_le_bytes());
        record[pos + 20..pos + 22].copy_from_slice(&24u16.to_le_bytes());
        record[pos + 24..pos + 28].copy_from_slice(&tag.to_le_bytes());
        record[pos + 28..pos + 30].copy_from_slice(&length.to_le_bytes());
        set_used(&mut record, (pos + 32) as u32);
        record
    }

    #[test]
    fn no_follow_symlinks_and_junctions_do_not_create_phantom_directories() {
        for tag in [0xA000_0003, 0xA000_000C] {
            let mut source = reader(reparse_record(tag, 0));
            assert!(source.entry(16).is_none());
            assert!(source.is_complete(), "known no-follow entry is intentionally omitted");
        }
    }

    #[test]
    fn provider_reparse_and_invalid_length_decline_the_entire_raw_result() {
        for record in [reparse_record(0x8000_0017, 0), reparse_record(0xA000_0003, 100)] {
            let mut source = reader(record);
            assert!(source.entry(16).is_none());
            assert!(!source.is_complete());
        }
    }

    #[test]
    fn missing_data_does_not_use_stale_file_name_sizes() {
        let mut record = dir_record(ROOT_RECORD, "missing-data");
        record[22..24].copy_from_slice(&1u16.to_le_bytes());
        record[64 + 24 + 48..64 + 24 + 56].copy_from_slice(&65536u64.to_le_bytes());
        let mut source = reader(record);
        assert!(source.entry(16).is_none());
        assert!(!source.is_complete());
    }

    #[test]
    fn lossy_utf16_name_never_merges_different_namespace_entries() {
        let mut record = file_record(ROOT_RECORD, "bad", 4096, 71, 1);
        record[64 + 24 + 66..64 + 24 + 68].copy_from_slice(&0xD800u16.to_le_bytes());
        let mut source = reader(record);
        assert!(source.entry(16).is_none());
        assert!(!source.is_complete());
    }
}
