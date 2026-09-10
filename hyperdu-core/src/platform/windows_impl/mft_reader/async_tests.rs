mod asynchronous {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct Trace {
        pending: bool,
        queued: Vec<u64>,
        finished: usize,
        cancelled: usize,
        reads_while_pending: usize,
    }
    struct Ahead {
        volume: MemVolume,
        trace: Arc<Mutex<Trace>>,
        pending: Option<(u64, usize)>,
        fail_ahead: bool,
        fail_required: bool,
    }
    impl VolumeSource for Ahead {
        fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> bool {
            if self.pending.is_some() {
                self.trace.lock().unwrap().reads_while_pending += 1;
            }
            if self.fail_required && offset >= MFT_LCN * CLUSTER as u64 + 32 * RECORD as u64 {
                return false;
            }
            self.volume.read_at(offset, buf)
        }
        fn begin_prefetch(&mut self, offset: u64, length: usize) -> bool {
            assert!(self.pending.is_none(), "bounded one pending request");
            self.pending = Some((offset, length));
            let mut trace = self.trace.lock().unwrap();
            trace.pending = true;
            trace.queued.push(offset);
            true
        }
        fn finish_prefetch(&mut self, buf: &mut [u8]) -> bool {
            let (offset, length) = self.pending.take().unwrap();
            assert_eq!(length, buf.len());
            let mut trace = self.trace.lock().unwrap();
            trace.pending = false;
            trace.finished += 1;
            drop(trace);
            if self.fail_ahead || self.fail_required {
                buf.fill(0xA5);
                return false;
            }
            self.volume.read_at(offset, buf)
        }
        fn cancel_prefetch(&mut self) {
            if self.pending.take().is_some() {
                let mut trace = self.trace.lock().unwrap();
                trace.pending = false;
                trace.cancelled += 1;
            }
        }
    }
    fn fixture(count: usize) -> Ahead {
        let users = (16..count)
            .map(|n| file_record(ROOT_RECORD, &format!("file-{n}"), 4096, n as u64, 1))
            .collect();
        let mut records = with_metadata_records(users, 1);
        let clusters = (count / 4) as u64;
        set_mft_extent_sizes(&mut records[0], 64, 0, clusters, CLUSTER as u64);
        records[0][128] = 0x14;
        records[0][129..133].copy_from_slice(&(clusters as u32).to_le_bytes());
        records[0][133] = MFT_LCN as u8;
        records[0][134] = 0;
        Ahead {
            volume: volume(records),
            trace: Arc::default(),
            pending: None,
            fail_ahead: false,
            fail_required: false,
        }
    }
    #[test]
    fn next_window_is_issued_before_current_entry_is_returned_and_random_read_preserves_it() {
        let mut source = fixture(80);
        let trace = source.trace.clone();
        let mut reader = MftReader::open(&mut source).unwrap();
        reader.window.limit = 16 * RECORD;
        assert_eq!(reader.entry(16).unwrap().name, "file-16");
        {
            let trace = trace.lock().unwrap();
            assert!(
                trace.pending,
                "next read is already in flight before caller can aggregate current entry"
            );
            assert_eq!(
                trace.queued,
                [MFT_LCN * CLUSTER as u64 + 32 * RECORD as u64]
            );
            assert_eq!(trace.finished, 0);
        }
        assert!(reader.read_record(70).is_some());
        assert!(trace.lock().unwrap().pending);
        assert_eq!(trace.lock().unwrap().reads_while_pending, 1);
        assert_eq!(reader.entry(32).unwrap().name, "file-32");
        assert_eq!(trace.lock().unwrap().finished, 1);
        assert!(trace.lock().unwrap().pending);
        drop(reader);
        assert!(!trace.lock().unwrap().pending);
        assert_eq!(trace.lock().unwrap().cancelled, 1);
    }
    #[test]
    fn prefetch_error_or_short_fill_retries_required_bytes_without_partial_cache() {
        for fail_ahead in [false, true] {
            let mut source = fixture(80);
            source.fail_ahead = fail_ahead;
            let expected = MftReader::open(MemVolume(source.volume.0.clone()))
                .unwrap()
                .entries();
            let trace = source.trace.clone();
            let mut reader = MftReader::open(source).unwrap();
            reader.window.limit = 16 * RECORD;
            assert_eq!(reader.entries(), expected);
            assert!(reader.is_complete());
            assert!(!trace.lock().unwrap().pending);
            assert!(trace.lock().unwrap().finished > 0);
        }
    }
    #[test]
    fn required_failure_remains_incomplete_and_no_finished_notification() {
        let mut source = fixture(80);
        source.fail_required = true;
        let trace = source.trace.clone();
        let mut reader = MftReader::open(source).unwrap();
        reader.window.limit = 16 * RECORD;
        let mut finished = false;
        let entries = reader
            .entries_with_control(|p| {
                finished |= p.finished;
                true
            })
            .unwrap();
        assert_eq!(entries.len(), 16);
        assert!(!reader.is_complete());
        assert!(!finished);
        assert!(!trace.lock().unwrap().pending);
    }
    #[test]
    fn cancellation_and_source_borrow_drain_before_return() {
        for initial in [false, true] {
            let source = fixture(2048);
            let trace = source.trace.clone();
            let mut reader = MftReader::open(source).unwrap();
            if initial {
                assert!(reader.entry(16).is_some());
            }
            assert!(reader
                .entries_with_control(|p| !initial && p.records < 256)
                .is_none());
            assert!(!reader.is_complete());
            assert!(!trace.lock().unwrap().pending);
            assert_eq!(trace.lock().unwrap().cancelled, 1);
        }
        let source = fixture(2048);
        let trace = source.trace.clone();
        let mut reader = MftReader::open(source).unwrap();
        assert!(reader.entry(16).is_some());
        let _source = reader.source_mut();
        assert!(!trace.lock().unwrap().pending);
    }
    #[test]
    fn next_physical_extent_is_queued_in_logical_order() {
        let mut source = fixture(80);
        // Split 20 clusters into 8 at LCN4 and 12 at LCN24, leaving a gap.
        let start = MFT_LCN as usize * CLUSTER;
        let old = source.volume.0.clone();
        source.volume.0.resize(36 * CLUSTER, 0xA5);
        source.volume.0[24 * CLUSTER..36 * CLUSTER]
            .copy_from_slice(&old[start + 8 * CLUSTER..start + 20 * CLUSTER]);
        source.volume.0[start + 128..start + 135].copy_from_slice(&[0x11, 8, 4, 0x11, 12, 20, 0]);
        let trace = source.trace.clone();
        let mut reader = MftReader::open(source).unwrap();
        reader.window.limit = 16 * RECORD;
        assert!(reader.entry(16).is_some());
        assert_eq!(trace.lock().unwrap().queued[0], 24 * CLUSTER as u64);
        assert_eq!(reader.entry(32).unwrap().name, "file-32");
        assert!(reader.is_complete());
    }
}
