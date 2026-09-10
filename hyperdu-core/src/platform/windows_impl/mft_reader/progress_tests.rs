mod progress_tests {
    use super::*;

    struct Reads {
        volume: MemVolume,
        calls: usize,
    }
    impl VolumeSource for Reads {
        fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> bool {
            self.calls += 1;
            self.volume.read_at(offset, buf)
        }
    }

    fn fixture(records: usize) -> Reads {
        let users = (16..records)
            .map(|number| file_record(ROOT_RECORD, &format!("file-{number}"), 4096, 71, 1))
            .collect();
        let mut records = with_metadata_records(users, 1);
        let clusters = records.len().div_ceil(CLUSTER / RECORD) as u32;
        set_mft_extent_sizes(&mut records[0], 64, 0, clusters as u64, CLUSTER as u64);
        records[0][128] = 0x14;
        records[0][129..133].copy_from_slice(&clusters.to_le_bytes());
        records[0][133] = MFT_LCN as u8;
        records[0][134] = 0;
        Reads {
            volume: volume(records),
            calls: 0,
        }
    }

    #[test]
    fn initial_control_can_abort_before_any_record_read() {
        let mut reader = MftReader::open(fixture(2048)).unwrap();
        let calls = reader.source.calls;
        let mut notifications = Vec::new();
        assert!(reader
            .entries_with_control(|p| {
                notifications.push(p);
                false
            })
            .is_none());
        assert_eq!(notifications, [ReadProgress::default()]);
        assert_eq!(reader.source.calls, calls);
        assert!(!reader.is_complete());
    }

    #[test]
    fn cadence_reports_files_and_final_count_without_changing_entries() {
        let mut reader = MftReader::open(fixture(1024)).unwrap();
        let mut notifications = Vec::new();
        let entries = reader
            .entries_with_control(|p| {
                notifications.push(p);
                true
            })
            .unwrap();
        let expected = MftReader::open(fixture(1024)).unwrap().entries();
        assert_eq!(entries, expected);
        assert_eq!(
            notifications.iter().map(|p| p.records).collect::<Vec<_>>(),
            [0, 256, 512, 768, 1008]
        );
        assert_eq!(
            notifications.last().unwrap(),
            &ReadProgress {
                records: 1008,
                files: 1008,
                finished: true
            }
        );
        assert!(notifications[..4].iter().all(|p| !p.finished));
    }

    #[test]
    fn cancelling_at_cadence_stops_reads_and_discards_partial_entries() {
        let mut reader = MftReader::open(fixture(4096)).unwrap();
        let mut notifications = Vec::new();
        assert!(reader
            .entries_with_control(|p| {
                notifications.push(p);
                p.records < 256
            })
            .is_none());
        assert_eq!(notifications.len(), 2);
        assert_eq!(
            reader.source.calls, 3,
            "bootstrap plus one bounded window only"
        );
        assert!(!reader.is_complete());
        assert!(!notifications.last().unwrap().finished);
    }

    #[test]
    fn final_file_count_excludes_directories_and_no_success_follows_corruption() {
        let valid = vec![
            dir_record(ROOT_RECORD, "dir"),
            file_record(ROOT_RECORD, "file", 4096, 1, 1),
        ];
        for corrupt in [false, true] {
            let mut records = valid.clone();
            if corrupt {
                records[1][RECORD - 2] ^= 1;
            }
            let mut reader = MftReader::open(volume(with_metadata_records(records, 5))).unwrap();
            let mut notifications = Vec::new();
            let entries = reader
                .entries_with_control(|p| {
                    notifications.push(p);
                    true
                })
                .unwrap();
            if corrupt {
                assert!(!reader.is_complete());
                assert!(notifications.iter().all(|p| !p.finished));
                assert_eq!(entries.len(), 1);
            } else {
                assert_eq!(
                    notifications.last().unwrap(),
                    &ReadProgress {
                        records: 4,
                        files: 1,
                        finished: true
                    }
                );
            }
        }
    }
}
