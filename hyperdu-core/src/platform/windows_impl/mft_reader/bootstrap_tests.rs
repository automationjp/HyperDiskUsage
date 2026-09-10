// Synthetic NTFS records: no raw volume access or administrator rights required.
mod bootstrap {
    use super::*;

    const BASE_SEQUENCE: u16 = 7;
    const EXTENSION_SEQUENCE: u16 = 9;
    const BASE_REFERENCE: u64 = (BASE_SEQUENCE as u64) << 48;

    fn reference(number: u64) -> u64 {
        number | ((EXTENSION_SEQUENCE as u64) << 48)
    }

    fn set_range(rec: &mut [u8], pos: usize, low: u64, clusters: u64) {
        rec[pos + 16..pos + 24].copy_from_slice(&low.to_le_bytes());
        rec[pos + 24..pos + 32].copy_from_slice(&(low + clusters - 1).to_le_bytes());
    }

    fn data_position(rec: &[u8]) -> usize {
        Attributes::new(rec, &parse_record_header(rec).unwrap())
            .find(|attr| attr.type_code == attr_type::DATA)
            .unwrap()
            .pos
    }

    fn fixture() -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        // The list is VCN ordered but the segment numbers run backwards.
        let mut base = mft_record_with_extensions(1, &[BASE_REFERENCE, reference(2), reference(1)]);
        base[16..18].copy_from_slice(&BASE_SEQUENCE.to_le_bytes());
        for (index, low) in [0u64, 1, 2].into_iter().enumerate() {
            let entry = 64 + 24 + index * 32;
            base[entry + 8..entry + 16].copy_from_slice(&low.to_le_bytes());
        }
        let data = data_position(&base);
        set_range(&mut base, data, 0, 1);
        for offset in [40, 48, 56] {
            base[data + offset..data + offset + 8]
                .copy_from_slice(&(3 * CLUSTER as u64).to_le_bytes());
        }
        let mut first = extension_record(1, 8);
        let mut second = extension_record(1, 12);
        for (record, low) in [(&mut first, 1), (&mut second, 2)] {
            record[16..18].copy_from_slice(&EXTENSION_SEQUENCE.to_le_bytes());
            record[32..40].copy_from_slice(&BASE_REFERENCE.to_le_bytes());
            set_range(record, 64, low, 1);
        }
        (base, first, second)
    }

    fn source(base: Vec<u8>, first: Vec<u8>, second: Vec<u8>) -> MemVolume {
        let mut vol = volume(vec![base, second, first]);
        vol.0.resize(13 * CLUSTER, 0);
        for (lcn, name) in [(8, "first-vcn"), (12, "second-vcn")] {
            vol.0[lcn * CLUSTER..lcn * CLUSTER + RECORD].copy_from_slice(&file_record(
                ROOT_RECORD,
                name,
                4096,
                73,
                1,
            ));
        }
        vol
    }

    fn rejected(vol: MemVolume) -> bool {
        MftReader::open(vol).map_or(true, |reader| !reader.is_complete())
    }

    #[test]
    fn bootstrap_follows_vcn_order_instead_of_segment_number() {
        let (base, first, second) = fixture();
        let mut reader = MftReader::open(source(base, first, second)).unwrap();
        assert!(reader.is_complete());
        assert_eq!(reader.record_count(), 12);
        assert_eq!(reader.entry(4).unwrap().name, "first-vcn");
        assert_eq!(reader.entry(8).unwrap().name, "second-vcn");
    }

    #[test]
    fn bootstrap_rejects_stale_segment_sequence() {
        let (base, mut first, second) = fixture();
        first[16..18].copy_from_slice(&(EXTENSION_SEQUENCE + 1).to_le_bytes());
        assert!(rejected(source(base, first, second)));
    }

    #[test]
    fn bootstrap_rejects_wrong_owner_sequence_or_number() {
        for owner in [BASE_REFERENCE + 1, ((BASE_SEQUENCE + 1) as u64) << 48] {
            let (base, mut first, second) = fixture();
            first[32..40].copy_from_slice(&owner.to_le_bytes());
            assert!(rejected(source(base, first, second)), "owner {owner}");
        }
    }

    #[test]
    fn bootstrap_checks_base_list_reference_instead_of_dropping_it() {
        let (mut base, first, second) = fixture();
        let entry = 64 + 24;
        base[entry + 16..entry + 24]
            .copy_from_slice(&(((BASE_SEQUENCE + 1) as u64) << 48).to_le_bytes());
        assert!(rejected(source(base, first, second)));
    }

    #[test]
    fn bootstrap_rejects_wrong_attribute_instance() {
        let (base, mut first, second) = fixture();
        first[64 + 14..64 + 16].copy_from_slice(&1u16.to_le_bytes());
        assert!(rejected(source(base, first, second)));
    }

    #[test]
    fn bootstrap_does_not_substitute_named_data_for_an_unnamed_extent() {
        let (base, mut first, second) = fixture();
        first[64 + 9] = 1;
        first[64 + 10..64 + 12].copy_from_slice(&68u16.to_le_bytes());
        first[64 + 68..64 + 70].copy_from_slice(&(b'x' as u16).to_le_bytes());
        assert!(rejected(source(base, first, second)));
    }

    #[test]
    fn bootstrap_never_appends_a_named_list_entry_to_the_mft_map() {
        let (mut base, first, second) = fixture();
        let entry = 64 + 24 + 32;
        base[entry + 6] = 1;
        base[entry + 7] = 26;
        base[entry + 26..entry + 28].copy_from_slice(&(b'x' as u16).to_le_bytes());
        assert!(rejected(source(base, first, second)));
    }

    #[test]
    fn bootstrap_rejects_vcn_gaps_overlaps_and_reference_disagreement() {
        for (list_low, extent_low) in [(2u64, 2u64), (0, 0), (1, 7)] {
            let (mut base, mut first, second) = fixture();
            let entry = 64 + 24 + 32;
            base[entry + 8..entry + 16].copy_from_slice(&list_low.to_le_bytes());
            set_range(&mut first, 64, extent_low, 1);
            assert!(
                rejected(source(base, first, second)),
                "VCNs {list_low}/{extent_low}"
            );
        }
    }

    #[test]
    fn bootstrap_rejects_run_length_that_disagrees_with_highest_vcn() {
        let (base, mut first, second) = fixture();
        first[64 + 24..64 + 32].copy_from_slice(&2u64.to_le_bytes());
        assert!(rejected(source(base, first, second)));
    }

    #[test]
    fn bootstrap_rejects_a_sparse_mft_mapping() {
        let (base, mut first, second) = fixture();
        first[128..132].copy_from_slice(&[0x01, 1, 0, 0]);
        assert!(rejected(source(base, first, second)));
    }

    #[test]
    fn bootstrap_run_terminator_must_be_inside_the_attribute() {
        let (base, mut first, second) = fixture();
        // A legal run ends exactly at this attribute's end without a terminator.
        first[68..72].copy_from_slice(&67u32.to_le_bytes());
        assert!(rejected(source(base, first, second)));
    }
    #[test]
    fn bootstrap_rejects_a_malformed_attribute_after_valid_data() {
        let mut base = mft_record(1);
        let end = parse_record_header(&base).unwrap().used_size as usize;
        base[end..end + 4].copy_from_slice(&attr_type::ATTRIBUTE_LIST.to_le_bytes());
        set_used(&mut base, (end + 8) as u32); // zero-length attribute is not EOF.
        assert!(rejected(volume(vec![base])));
    }

    #[test]
    fn bootstrap_keeps_distinct_extents_in_the_same_segment() {
        let (mut base, mut first, _) = fixture();
        let entry = 64 + 24 + 2 * 32;
        base[entry + 16..entry + 24].copy_from_slice(&reference(2).to_le_bytes());
        base[entry + 24..entry + 26].copy_from_slice(&1u16.to_le_bytes());
        let mut second = extension_record(1, 12);
        set_range(&mut second, 64, 2, 1);
        second[64 + 14..64 + 16].copy_from_slice(&1u16.to_le_bytes());
        first[136..208].copy_from_slice(&second[64..136]);
        set_used(&mut first, 208);
        let mut reader = MftReader::open(source(base, first, blank_record(0, 0))).unwrap();
        assert!(reader.is_complete());
        assert_eq!(reader.entry(4).unwrap().name, "first-vcn");
        assert_eq!(reader.entry(8).unwrap().name, "second-vcn");
    }

    #[test]
    fn bootstrap_resolves_an_extension_made_reachable_by_the_previous_vcn() {
        let (mut base, first, second) = fixture();
        let entry = 64 + 24 + 2 * 32;
        base[entry + 16..entry + 24].copy_from_slice(&reference(4).to_le_bytes());
        let mut vol = source(base, first, blank_record(0, 0));
        vol.0[8 * CLUSTER..8 * CLUSTER + RECORD].copy_from_slice(&second);
        let mut reader = MftReader::open(vol).unwrap();
        assert!(reader.is_complete());
        assert_eq!(reader.entry(8).unwrap().name, "second-vcn");
    }

    #[test]
    fn bootstrap_ignores_named_data_before_the_unnamed_base_extent() {
        let mut base = mft_record(1);
        base.copy_within(64..136, 136);
        base[64 + 9] = 1;
        base[64 + 10..64 + 12].copy_from_slice(&68u16.to_le_bytes());
        base[64 + 68..64 + 70].copy_from_slice(&(b'x' as u16).to_le_bytes());
        set_used(&mut base, 208);
        let reader = MftReader::open(volume(vec![base])).unwrap();
        assert!(reader.is_complete());
        assert_eq!(reader.mft_clusters(), 1);
    }
}
