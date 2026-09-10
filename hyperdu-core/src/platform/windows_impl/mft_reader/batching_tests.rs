mod batching {
    use super::*;

    #[test]
    fn required_unreadable_torn_or_bad_signature_records_invalidate_completeness() {
        for failure in 0..3 {
            let mut rec = file_record(ROOT_RECORD, "required", 4096, 100, 1);
            match failure {
                1 => rec[RECORD - 2] ^= 1,
                2 => rec[..4].copy_from_slice(b"BAAD"),
                _ => {}
            }
            let mut vol = volume(with_metadata_records(vec![rec], 5));
            if failure == 0 {
                vol.0
                    .truncate(MFT_LCN as usize * CLUSTER + 16 * RECORD + RECORD / 2);
            }
            let mut reader = MftReader::open(vol).unwrap();
            assert!(reader.entries().is_empty());
            assert!(!reader.is_complete(), "failure {failure}");
        }
    }

    #[test]
    fn unresolved_mft_extension_invalidates_completeness() {
        for record in [1, 99] {
            let vol = volume(vec![mft_record_with_extensions(1, &[record])]);
            let reader = MftReader::open(vol).unwrap();
            assert!(!reader.is_complete(), "extension {record}");
        }
    }

    #[test]
    fn valid_inactive_and_zero_slots_are_not_read_failures() {
        let vol = volume(with_metadata_records(vec![blank_record(0, 0)], 5));
        let mut reader = MftReader::open(vol).unwrap();
        assert!(reader.entries().is_empty());
        assert!(reader.is_complete());
    }

    struct Counted<S> {
        inner: S,
        calls: usize,
        bytes: usize,
        maximum: usize,
        io_time: std::time::Duration,
    }

    impl<S> Counted<S> {
        fn new(inner: S) -> Self {
            Self {
                inner,
                calls: 0,
                bytes: 0,
                maximum: 0,
                io_time: std::time::Duration::ZERO,
            }
        }
    }

    impl<S: VolumeSource> VolumeSource for Counted<S> {
        fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> bool {
            self.calls += 1;
            self.bytes += buf.len();
            self.maximum = self.maximum.max(buf.len());
            let start = std::time::Instant::now();
            let result = self.inner.read_at(offset, buf);
            self.io_time += start.elapsed();
            result
        }
    }

    fn large_volume(count: usize) -> MemVolume {
        assert_eq!(count % 4, 0);
        let user = (16..count)
            .map(|n| file_record(ROOT_RECORD, &format!("file-{n}"), 4096, n as u64, 1))
            .collect();
        let mut records = with_metadata_records(user, 1);
        // Four-byte run length keeps this fixture useful past 255 clusters.
        set_mft_extent_sizes(&mut records[0], 64, 0, (count / 4) as u64, CLUSTER as u64);
        records[0][128] = 0x14;
        records[0][129..133].copy_from_slice(&((count / 4) as u32).to_le_bytes());
        records[0][133] = MFT_LCN as u8;
        records[0][134] = 0;
        volume(records)
    }

    #[test]
    fn sequential_windows_match_uncached_entries_and_maps_with_bounded_reads() {
        let bytes = large_volume(8192).0;
        let mut plain = MftReader::open(Counted::new(MemVolume(bytes.clone()))).unwrap();
        plain.window.limit = 0;
        let expected = plain.entries();
        let mut batched = MftReader::open(Counted::new(MemVolume(bytes))).unwrap();
        let actual = batched.entries();
        assert_eq!(actual, expected);
        assert!(plain.is_complete() && batched.is_complete());
        let actual_map = to_stat_map(&actual, &paths_for(&actual), "C:\\", false, true);
        let expected_map = to_stat_map(&expected, &paths_for(&expected), "C:\\", false, true);
        assert_eq!(actual_map.len(), expected_map.len());
        for (path, actual) in actual_map {
            let expected = &expected_map[&path];
            assert_eq!(
                (actual.files, actual.logical, actual.physical),
                (expected.files, expected.logical, expected.physical)
            );
        }
        assert_eq!(plain.source.calls, 2 + 8192 - 16);
        assert_eq!(batched.source.calls, 2 + 8);
        assert!(batched.source.maximum <= 1024 * 1024);
        assert_eq!(batched.source.bytes, plain.source.bytes);
    }

    #[test]
    fn a_split_record_follows_the_next_extent_and_fixups_never_mutate_cache() {
        let mut boot = boot_sector();
        boot[13] = 1; // 512-byte clusters, 1024-byte records.
        let mut mft = mft_record(1);
        set_mft_extent_sizes(&mut mft, 64, 0, 6, SECTOR as u64);
        mft[128..135].copy_from_slice(&[0x11, 3, 4, 0x11, 3, 8, 0]);
        let file = file_record(ROOT_RECORD, "split", 4096, 73, 1);
        boot.resize(4 * SECTOR, 0);
        boot.extend_from_slice(&mft);
        boot.extend_from_slice(&file[..SECTOR]);
        boot.resize(12 * SECTOR, 0xA5); // physical gap must never be read as record data.
        boot.extend_from_slice(&file[SECTOR..]);
        boot.resize(15 * SECTOR, 0);
        for limit in [0, 1024 * 1024] {
            let mut reader = MftReader::open(Counted::new(MemVolume(boot.clone()))).unwrap();
            reader.window.limit = limit;
            let first = reader.entry(1).unwrap();
            assert_eq!(first.name, "split");
            assert_eq!(first.sizes.real_size, 73);
            assert_eq!(reader.entry(1), Some(first));
            assert!(reader.read_record(1).is_some());
            assert!(reader.is_complete());
            assert!(reader.source.maximum <= 3 * SECTOR);
        }
    }

    struct PartialFailure(MemVolume);
    impl VolumeSource for PartialFailure {
        fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> bool {
            // Simulate a backend that partially fills the buffer before failure.
            if buf.len() > RECORD {
                buf.fill(0xA5);
                return false;
            }
            self.0.read_at(offset, buf)
        }
    }

    #[test]
    fn failed_read_ahead_retries_only_required_bytes_without_stale_cache() {
        let volume = large_volume(20);
        let mut reader = MftReader::open(PartialFailure(volume)).unwrap();
        assert_eq!(reader.entries().len(), 4);
        assert!(reader.is_complete());
    }

    #[test]
    fn random_extension_misses_preserve_the_sequential_window() {
        let (base, ext) = data_extension_fixture(false);
        let mut vol = large_volume(2048);
        let start = MFT_LCN as usize * CLUSTER;
        vol.0[start + 16 * RECORD..start + 17 * RECORD].copy_from_slice(&base);
        vol.0[start + 17 * RECORD..start + 18 * RECORD].copy_from_slice(&ext);
        let mut reader = MftReader::open(Counted::new(vol)).unwrap();
        assert!(reader.entry(16).is_some());
        let calls = reader.source.calls;
        assert!(reader.read_record(1800).is_some());
        assert_eq!(reader.source.calls, calls + 1);
        assert!(reader.entry(18).is_some());
        assert_eq!(reader.source.calls, calls + 1);
        assert!(reader.entry(16).is_some());
        assert_eq!(reader.source.calls, calls + 1);
    }

    #[test]
    fn unsupported_or_malformed_bootstrap_attribute_lists_are_rejected() {
        for nonresident in [false, true] {
            let mut mft = mft_record_with_extensions(1, &[1]);
            if nonresident {
                mft[72] = 1;
            } else {
                mft[64 + 24 + 4..64 + 24 + 6].copy_from_slice(&0u16.to_le_bytes());
            }
            assert!(MftReader::open(volume(vec![mft])).is_none());
        }
    }

    struct FileVolume(std::fs::File);
    impl VolumeSource for FileVolume {
        fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> bool {
            use std::io::{Read, Seek, SeekFrom};
            self.0.seek(SeekFrom::Start(offset)).is_ok() && self.0.read_exact(buf).is_ok()
        }
    }

    fn measure<S: VolumeSource>(source: S, limit: usize, label: &str) -> Vec<Entry> {
        let start = std::time::Instant::now();
        let mut reader = MftReader::open(Counted::new(source)).unwrap();
        reader.window.limit = limit;
        let first = reader.entry(16).unwrap();
        let first_time = start.elapsed();
        let entries = reader.entries();
        let scan = start.elapsed();
        assert_eq!(entries.first(), Some(&first));
        assert!(reader.is_complete());
        let paths = paths_for(&entries);
        let map = to_stat_map(&entries, &paths, "C:\\", false, true);
        let elapsed = start.elapsed();
        assert_eq!(
            map.values().map(|stat| stat.files).sum::<u64>(),
            entries.len() as u64
        );
        eprintln!("mft_fixture source={label} window={limit} entries={} calls={} bytes={} max_request={} io_ns={} first_ns={} scan_ns={} paths_map_ns={} total_ns={} cache_capacity={}",
                  entries.len(), reader.source.calls, reader.source.bytes, reader.source.maximum,
                  reader.source.io_time.as_nanos(), first_time.as_nanos(), scan.as_nanos(),
                  (elapsed-scan).as_nanos(), elapsed.as_nanos(), reader.window.capacity());
        entries
    }

    #[test]
    #[ignore = "release fixture benchmark; run separately from builds and filesystem benchmarks"]
    fn benchmark_memory_and_file_read_windows() {
        use std::io::Write;
        let bytes = large_volume(32768).0;
        let expected = MftReader::open(MemVolume(bytes.clone())).unwrap().entries();
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(&bytes).unwrap();
        file.sync_all().unwrap();
        for round in 0..6 {
            let policies = if round % 2 == 0 {
                [0, 1024 * 1024]
            } else {
                [1024 * 1024, 0]
            };
            for limit in policies {
                assert_eq!(measure(MemVolume(bytes.clone()), limit, "memory"), expected);
                assert_eq!(
                    measure(FileVolume(file.try_clone().unwrap()), limit, "file"),
                    expected
                );
            }
        }
    }

    #[cfg(all(windows, target_env = "msvc"))]
    #[test]
    #[ignore = "requires explicit HYPERDU_MFT_BENCH_DRIVE for a caller-owned, quiescent NTFS fixture"]
    fn benchmark_explicit_ntfs_volume_read_windows() {
        let drive = std::env::var("HYPERDU_MFT_BENCH_DRIVE")
            .expect("set HYPERDU_MFT_BENCH_DRIVE to the owned fixture drive letter");
        assert!(drive.len() == 1 && drive.as_bytes()[0].is_ascii_alphabetic());
        let letter = drive.chars().next().unwrap();
        let prefix = format!("{letter}:\\");
        let mut expected = None;
        for round in 0..6 {
            let policies = if round % 2 == 0 {
                [0, 1024 * 1024]
            } else {
                [1024 * 1024, 0]
            };
            for limit in policies {
                let start = std::time::Instant::now();
                let source =
                    WindowsVolume::open(letter).expect("open explicit NTFS fixture volume");
                let mut reader =
                    MftReader::open(Counted::new(source)).expect("read NTFS bootstrap");
                let sector = reader.geometry().bytes_per_sector;
                reader.source_mut().inner.set_sector_size(sector);
                reader.window.limit = limit;
                let entries = reader.entries();
                let scan = start.elapsed();
                assert!(reader.is_complete(), "required NTFS records are unresolved");
                assert!(!entries.is_empty(), "fixture must contain user records");
                let paths = paths_for(&entries);
                let map = to_stat_map(&entries, &paths, &prefix, false, true)
                    .into_iter()
                    .map(|(path, stat)| (path, (stat.files, stat.logical, stat.physical)))
                    .collect::<std::collections::BTreeMap<_, _>>();
                let elapsed = start.elapsed();
                eprintln!("mft_volume drive={letter} round={round} window={limit} entries={} calls={} bytes={} max_request={} io_ns={} scan_ns={} paths_map_ns={} total_ns={} cache_capacity={}",
                    entries.len(), reader.source.calls, reader.source.bytes, reader.source.maximum,
                    reader.source.io_time.as_nanos(), scan.as_nanos(), (elapsed-scan).as_nanos(),
                    elapsed.as_nanos(), reader.window.capacity());
                let actual = (entries, map);
                if let Some(expected) = &expected {
                    assert_eq!(
                        &actual, expected,
                        "quiescent fixture must match exactly between policies"
                    );
                } else {
                    expected = Some(actual);
                }
            }
        }
    }
}
