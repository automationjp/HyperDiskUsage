//! Explicit-offset read-ahead. Each operation owns stable storage and its own
//! event; cancellation always drains that original operation before destruction.
use std::{
    cell::UnsafeCell,
    ffi::OsStr,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    sync::Arc,
};

use windows::{
    core::PCWSTR,
    Win32::{
        Foundation::{
            ERROR_IO_INCOMPLETE, ERROR_IO_PENDING, ERROR_NOT_FOUND, GENERIC_READ, HANDLE,
        },
        Storage::FileSystem::{
            CreateFileW, ReadFile, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_NO_BUFFERING,
            FILE_FLAG_OVERLAPPED, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
        },
        System::{
            Ioctl::{
                PropertyStandardQuery, StorageAccessAlignmentProperty, GET_LENGTH_INFORMATION,
                IOCTL_DISK_GET_LENGTH_INFO, IOCTL_STORAGE_QUERY_PROPERTY,
                STORAGE_ACCESS_ALIGNMENT_DESCRIPTOR, STORAGE_PROPERTY_QUERY,
            },
            Threading::CreateEventW,
            IO::{
                CancelIoEx, DeviceIoControl, GetOverlappedResult, OVERLAPPED, OVERLAPPED_0,
                OVERLAPPED_0_0,
            },
        },
    },
};

const MAX_WINDOW: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Mode {
    Auto,
    Sync,
    Overlapped,
    Unbuffered,
}
impl Mode {
    fn parse(value: Option<&OsStr>) -> Option<Self> {
        match value {
            None => Some(Self::Auto),
            Some(v) if v == "auto" => Some(Self::Auto),
            Some(v) if v == "sync" => Some(Self::Sync),
            Some(v) if v == "overlapped" => Some(Self::Overlapped),
            Some(v) if v == "unbuffered" => Some(Self::Unbuffered),
            Some(_) => None,
        }
    }
    pub(super) fn configured() -> Self {
        Self::parse(std::env::var_os("HYPERDU_MFT_IO").as_deref()).unwrap_or_else(|| {
            eprintln!("hyperdu: invalid HYPERDU_MFT_IO (expected auto, sync, overlapped or unbuffered); using sync");
            Self::Sync
        })
    }
}

fn handle(owner: &OwnedHandle) -> HANDLE {
    HANDLE(owner.as_raw_handle())
}

/// Only validated OS geometry may widen a transfer beyond its requested range.
#[derive(Clone, Copy, Debug)]
struct Bounds {
    sector: u64,
    alignment: usize,
    length: u64,
}
impl Bounds {
    fn checked(sector: u32, physical: u32, length: i64) -> Option<Self> {
        if !(512..=65536).contains(&sector)
            || !sector.is_power_of_two()
            || physical < sector
            || physical > 65536
            || !physical.is_power_of_two()
            || length <= 0
            || length as u64 % sector as u64 != 0
        {
            return None;
        }
        Some(Self {
            sector: sector as u64,
            alignment: physical as usize,
            length: length as u64,
        })
    }
    fn range(self, offset: u64, length: usize) -> Option<(u64, usize, usize)> {
        if length == 0 || length > MAX_WINDOW {
            return None;
        }
        let start = offset - offset % self.sector;
        let end = offset
            .checked_add(length as u64)?
            .checked_add(self.sector - 1)?
            / self.sector
            * self.sector;
        if end > self.length || end > i64::MAX as u64 {
            return None;
        }
        let span = usize::try_from(end.checked_sub(start)?).ok()?;
        Some((start, span, usize::try_from(offset - start).ok()?))
    }
}

/// Query synchronously on the separate bootstrap/random-read handle.
fn volume_bounds(raw: HANDLE) -> Option<Bounds> {
    let query = STORAGE_PROPERTY_QUERY {
        PropertyId: StorageAccessAlignmentProperty,
        QueryType: PropertyStandardQuery,
        ..Default::default()
    };
    let mut alignment = STORAGE_ACCESS_ALIGNMENT_DESCRIPTOR::default();
    let mut length = GET_LENGTH_INFORMATION::default();
    let mut returned = 0;
    // SAFETY: every pointer refers to a live correctly sized input/output value;
    // the handle is synchronous and borrowed for these calls only.
    unsafe {
        DeviceIoControl(
            raw,
            IOCTL_STORAGE_QUERY_PROPERTY,
            Some((&query as *const STORAGE_PROPERTY_QUERY).cast()),
            std::mem::size_of_val(&query) as u32,
            Some((&mut alignment as *mut STORAGE_ACCESS_ALIGNMENT_DESCRIPTOR).cast()),
            std::mem::size_of_val(&alignment) as u32,
            Some(&mut returned),
            None,
        )
        .ok()?;
        if returned < std::mem::size_of_val(&alignment) as u32
            || alignment.Size < std::mem::size_of_val(&alignment) as u32
            || alignment.Version < std::mem::size_of_val(&alignment) as u32
        {
            return None;
        }
        DeviceIoControl(
            raw,
            IOCTL_DISK_GET_LENGTH_INFO,
            None,
            0,
            Some((&mut length as *mut GET_LENGTH_INFORMATION).cast()),
            std::mem::size_of_val(&length) as u32,
            Some(&mut returned),
            None,
        )
        .ok()?;
        if returned < std::mem::size_of_val(&length) as u32 {
            return None;
        }
    }
    Bounds::checked(
        alignment.BytesPerLogicalSector,
        alignment.BytesPerPhysicalSector,
        length.Length,
    )
}

pub(super) struct Engine {
    handle: Arc<OwnedHandle>,
    bounds: Bounds,
    pending: Option<Operation>,
    effective: &'static str,
    submitted: u64,
    completed: u64,
    cancelled: u64,
}
impl Engine {
    pub(super) fn open(path: &[u16], sync_handle: HANDLE, mode: Mode) -> Option<Self> {
        if mode == Mode::Sync {
            return None;
        }
        let Some(bounds) = volume_bounds(sync_handle) else {
            log::warn!("MFT async geometry unavailable; using synchronous reads");
            return None;
        };
        let unbuffered = mode == Mode::Unbuffered;
        let open = |unbuffered| {
            let mut flags = FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OVERLAPPED;
            if unbuffered {
                flags |= FILE_FLAG_NO_BUFFERING;
            }
            // SAFETY: path is the same live NUL-terminated volume path used for
            // the bootstrap handle. The resulting handle has exactly one owner.
            unsafe {
                CreateFileW(
                    PCWSTR(path.as_ptr()),
                    GENERIC_READ.0,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    None,
                    OPEN_EXISTING,
                    flags,
                    None,
                )
            }
            .ok()
            .map(|h| {
                // SAFETY: successful CreateFileW transfers unique ownership.
                unsafe { OwnedHandle::from_raw_handle(h.0) }
            })
        };
        let mut effective = if unbuffered {
            "unbuffered"
        } else {
            "overlapped"
        };
        let owner = open(unbuffered).or_else(|| {
            if unbuffered {
                log::warn!("MFT unbuffered open unavailable; using buffered overlapped reads");
                effective = "overlapped";
                open(false)
            } else {
                None
            }
        });
        let Some(owner) = owner else {
            log::warn!("MFT overlapped open unavailable; using synchronous reads");
            return None;
        };
        log::debug!(
            "MFT I/O requested={mode:?} effective={effective} sector={} alignment={} volume_bytes={}",
            bounds.sector,
            bounds.alignment,
            bounds.length
        );
        Some(Self {
            handle: Arc::new(owner),
            bounds,
            pending: None,
            effective,
            submitted: 0,
            completed: 0,
            cancelled: 0,
        })
    }
    pub(super) fn begin(&mut self, offset: u64, length: usize) -> bool {
        self.cancel();
        self.pending = Operation::start(self.handle.clone(), self.bounds, offset, length);
        let queued = self.pending.is_some();
        self.submitted += u64::from(queued);
        queued
    }
    pub(super) fn finish(&mut self, bytes: &mut [u8]) -> bool {
        let success = self
            .pending
            .take()
            .is_some_and(|mut op| op.copy_result(bytes));
        self.completed += u64::from(success);
        success
    }
    pub(super) fn cancel(&mut self) {
        self.cancelled += u64::from(self.pending.is_some());
        self.pending = None;
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.cancel();
        log::debug!(
            "MFT read-ahead effective={} submitted={} completed={} cancelled={}",
            self.effective,
            self.submitted,
            self.completed,
            self.cancelled
        );
    }
}

/// The buffer and OVERLAPPED addresses survive moves of this owner. UnsafeCell
/// explicitly permits the OS to mutate OVERLAPPED while Rust owns the allocation.
struct Operation {
    handle: Arc<OwnedHandle>,
    event: OwnedHandle,
    overlapped: Box<UnsafeCell<OVERLAPPED>>,
    storage: Box<[u8]>,
    aligned: usize,
    span: usize,
    within: usize,
    wanted: usize,
    pending: bool,
}
impl Operation {
    fn start(owner: Arc<OwnedHandle>, bounds: Bounds, offset: u64, length: usize) -> Option<Self> {
        let (start, span, within) = bounds.range(offset, length)?;
        // Allocate spare bytes to align the stable boxed backing allocation.
        let storage = vec![0u8; span.checked_add(bounds.alignment - 1)?].into_boxed_slice();
        let aligned =
            (bounds.alignment - storage.as_ptr() as usize % bounds.alignment) % bounds.alignment;
        // SAFETY: unnamed manual-reset event, no borrowed security descriptor.
        let event = unsafe { CreateEventW(None, true, false, PCWSTR::null()) }.ok()?;
        // SAFETY: CreateEventW returned a uniquely owned valid handle.
        let event = unsafe { OwnedHandle::from_raw_handle(event.0) };
        let ov = OVERLAPPED {
            hEvent: handle(&event),
            Anonymous: OVERLAPPED_0 {
                Anonymous: OVERLAPPED_0_0 {
                    Offset: start as u32,
                    OffsetHigh: (start >> 32) as u32,
                },
            },
            ..Default::default()
        };
        let mut op = Self {
            handle: owner,
            event,
            overlapped: Box::new(UnsafeCell::new(ov)),
            storage,
            aligned,
            span,
            within,
            wanted: length,
            pending: false,
        };
        // SAFETY: both allocations remain stable and untouched until completion.
        // The request has explicit offsets, owned event, bounded aligned range,
        // and the handle was opened for OVERLAPPED I/O. No pointer escapes owner.
        let read = unsafe {
            ReadFile(
                handle(&op.handle),
                Some(&mut op.storage[aligned..aligned + span]),
                None,
                Some(op.overlapped.get()),
            )
        };
        match read {
            Ok(()) => {
                op.pending = true;
                Some(op)
            }
            Err(error) if error.code() == ERROR_IO_PENDING.to_hresult() => {
                op.pending = true;
                Some(op)
            }
            Err(error) => {
                log::debug!("MFT read-ahead submission failed: {error}");
                None
            }
        }
    }
    fn wait(&mut self) -> Option<usize> {
        let mut bytes = 0;
        loop {
            // SAFETY: the original handle/event/OVERLAPPED/buffer are all still
            // owned here, and bWait=true establishes completion before reuse.
            let result = unsafe {
                GetOverlappedResult(
                    handle(&self.handle),
                    self.overlapped.get(),
                    &mut bytes,
                    true,
                )
            };
            if let Err(error) = &result {
                if error.code() == ERROR_IO_INCOMPLETE.to_hresult() {
                    continue;
                }
            }
            self.pending = false;
            return match result {
                Ok(()) => Some(bytes as usize),
                Err(error) => {
                    log::debug!("MFT read-ahead completion failed: {error}");
                    None
                }
            };
        }
    }
    fn copy_result(&mut self, out: &mut [u8]) -> bool {
        if self.wait() != Some(self.span) || out.len() != self.wanted {
            return false;
        }
        let start = self.aligned + self.within;
        out.copy_from_slice(&self.storage[start..start + self.wanted]);
        true
    }
}
impl Drop for Operation {
    fn drop(&mut self) {
        if self.pending {
            // SAFETY: cancel this exact operation only. Even ERROR_NOT_FOUND is
            // a race, not proof of completion; always wait on the original below.
            if let Err(error) =
                unsafe { CancelIoEx(handle(&self.handle), Some(self.overlapped.get())) }
            {
                if error.code() != ERROR_NOT_FOUND.to_hresult() {
                    log::debug!("MFT read-ahead cancellation request failed: {error}");
                }
            }
            let _completed = self.wait();
        }
        // Keep the event visibly owned through the drain, before automatic drop.
        let _event = &self.event;
    }
}
#[cfg(test)]
mod tests {
    use std::{fs::OpenOptions, io::Write, os::windows::fs::OpenOptionsExt};

    use super::*;

    fn file(unbuffered: bool) -> (tempfile::NamedTempFile, Arc<OwnedHandle>, Vec<u8>, Bounds) {
        let bytes: Vec<u8> = (0..32768).map(|n| (n % 251) as u8).collect();
        let mut fixture = tempfile::NamedTempFile::new().unwrap();
        fixture.write_all(&bytes).unwrap();
        fixture.flush().unwrap();
        let flags = FILE_FLAG_OVERLAPPED
            | if unbuffered {
                FILE_FLAG_NO_BUFFERING
            } else {
                Default::default()
            };
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(flags.0)
            .open(fixture.path())
            .unwrap();
        // A 4K transfer unit works for both 512-byte and 4K-native test volumes;
        // the production path uses queried geometry instead of this fixture unit.
        let bounds = Bounds::checked(4096, 65536, bytes.len() as i64).unwrap();
        (fixture, Arc::new(file.into()), bytes, bounds)
    }

    #[test]
    fn aligned_ranges_reject_overflow_out_of_volume_and_unbounded_requests() {
        let b = Bounds::checked(512, 4096, 4096).unwrap();
        assert_eq!(b.range(513, 10), Some((512, 512, 1)));
        assert_eq!(b.range(4095, 1), Some((3584, 512, 511)));
        for (offset, length) in [(4095, 2), (u64::MAX, 1), (0, 0), (0, MAX_WINDOW + 1)] {
            assert!(b.range(offset, length).is_none());
        }
        for (sector, physical, length) in [
            (0, 512, 4096),
            (513, 4096, 4096),
            (4096, 512, 4096),
            (512, 131072, 4096),
            (512, 4096, -1),
            (512, 4096, 4095),
        ] {
            assert!(Bounds::checked(sector, physical, length).is_none());
        }
        let high = Bounds::checked(4096, 4096, (1i64 << 33) + 4096).unwrap();
        assert_eq!(
            high.range((1u64 << 32) + 1, 1024),
            Some((1u64 << 32, 4096, 1))
        );
    }

    #[test]
    fn io_modes_are_explicit_and_invalid_input_cannot_select_async() {
        assert_eq!(Mode::parse(None), Some(Mode::Auto));
        for (name, expected) in [
            ("auto", Mode::Auto),
            ("sync", Mode::Sync),
            ("overlapped", Mode::Overlapped),
            ("unbuffered", Mode::Unbuffered),
        ] {
            assert_eq!(Mode::parse(Some(OsStr::new(name))), Some(expected));
        }
        for name in ["", "AVX2", "overlap", " unbuffered"] {
            assert_eq!(Mode::parse(Some(OsStr::new(name))), None);
        }
    }

    #[test]
    fn native_overlapped_explicit_offsets_survive_owner_moves_and_reject_short_read() {
        let (_fixture, owner, bytes, bounds) = file(false);
        let op = Operation::start(owner.clone(), bounds, 777, 1234).unwrap();
        let original = op.overlapped.get();
        let mut moved = Box::new(op);
        assert_eq!(original, moved.overlapped.get());
        let mut actual = vec![0; 1234];
        assert!(moved.copy_result(&mut actual));
        assert_eq!(actual, bytes[777..2011]);
        assert!(!moved.pending);
        // Claimed geometry exceeds actual EOF only in this fault-injection test.
        let oversized = Bounds {
            length: bounds.length * 2,
            ..bounds
        };
        if let Some(mut short) = Operation::start(owner, oversized, bounds.length - 512, 1024) {
            let mut out = vec![0x5A; 1024];
            assert!(!short.copy_result(&mut out));
            assert_eq!(out, vec![0x5A; 1024], "partial buffer is never exposed");
            assert!(!short.pending);
        }
    }

    #[test]
    fn native_cancel_or_already_completed_race_drains_original_and_handle_remains_usable() {
        let (_fixture, owner, bytes, bounds) = file(false);
        for _ in 0..32 {
            let op = Operation::start(owner.clone(), bounds, 0, bytes.len()).unwrap();
            // Drop invokes CancelIoEx followed by GetOverlappedResult(TRUE),
            // including when this fast local request already completed.
            drop(op);
        }
        let mut op = Operation::start(owner, bounds, 0, bytes.len()).unwrap();
        let mut out = vec![0; bytes.len()];
        assert!(op.copy_result(&mut out));
        assert_eq!(out, bytes);
    }

    #[test]
    fn native_unbuffered_reads_use_stable_aligned_buffer() {
        let (_fixture, owner, bytes, bounds) = file(true);
        let mut op = Operation::start(owner, bounds, 4097, 511).unwrap();
        assert_eq!(
            (op.storage.as_ptr() as usize + op.aligned) % bounds.alignment,
            0
        );
        let mut out = vec![0; 511];
        assert!(op.copy_result(&mut out));
        assert_eq!(out, bytes[4097..4608]);
    }
}

#[cfg(test)]
mod raw_volume_tests {
    use super::{
        super::{MftReader, WindowsVolume},
        *,
    };

    #[test]
    fn owned_raw_volume_pipeline_executes_each_mode_and_drains_on_cancel() {
        let Some(root) = std::env::var_os("HYPERDU_MFT_PARITY_ROOT") else {
            eprintln!("NOT RUN: raw-volume pipeline requires the owned immutable NTFS fixture");
            return;
        };
        if std::env::var_os("HYPERDU_MFT_PARITY_FIXTURE").is_none() {
            eprintln!("NOT RUN: pipeline proof is restricted to the owned fixture");
            return;
        }
        let drive = root.to_string_lossy().chars().next().unwrap();
        let path: Vec<_> = format!(r"\\.\{drive}:")
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let mut sync = WindowsVolume::open(drive).expect("owned volume must be readable");
        sync.prefetch = None;
        let mut reader = MftReader::open(sync).expect("owned MFT bootstrap");
        let expected = reader.entries();
        assert!(reader.is_complete());
        assert!(
            expected.len() > 2048,
            "fixture must cross the 1 MiB MFT window"
        );
        for (mode, effective) in [
            (Mode::Overlapped, "overlapped"),
            (Mode::Unbuffered, "unbuffered"),
        ] {
            let mut source = WindowsVolume::open(drive).unwrap();
            source.prefetch = Some(
                Engine::open(&path, source.handle, mode)
                    .expect("fixture must support queried geometry and native async I/O"),
            );
            let mut reader = MftReader::open(source).unwrap();
            assert_eq!(reader.entries(), expected, "raw volume mode {mode:?}");
            assert!(reader.is_complete());
            let engine = reader.source.prefetch.as_ref().unwrap();
            assert_eq!(engine.effective, effective);
            assert!(engine.submitted > 0 && engine.completed > 0);
            assert!(engine.pending.is_none());
            eprintln!(
                "raw pipeline {effective}: submitted={} completed={}",
                engine.submitted, engine.completed
            );

            let mut source = WindowsVolume::open(drive).unwrap();
            source.prefetch = Some(Engine::open(&path, source.handle, mode).unwrap());
            let mut reader = MftReader::open(source).unwrap();
            assert!(reader
                .entries_with_control(|progress| progress.records < 256)
                .is_none());
            let engine = reader.source.prefetch.as_ref().unwrap();
            assert!(engine.pending.is_none());
            assert!(engine.cancelled > 0);
        }
    }

    #[test]
    #[ignore = "release benchmark on an explicitly owned immutable NTFS fixture"]
    #[allow(clippy::assertions_on_constants)] // Guard explicit benchmark invocations in debug builds.
    fn benchmark_owned_raw_pipeline() {
        use std::{collections::BTreeMap, time::Instant};

        use super::super::{paths_for, to_stat_map};

        assert!(!cfg!(debug_assertions), "benchmark requires --release");
        assert_eq!(
            std::env::var("HYPERDU_MFT_PARITY_FIXTURE").as_deref(),
            Ok("1")
        );
        let root = std::env::var("HYPERDU_MFT_PARITY_ROOT").expect("owned fixture root");
        let drive = root.chars().next().unwrap();
        assert!(drive.is_ascii_alphabetic());
        let path: Vec<_> = format!(r"\\.\{drive}:")
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let policies = [
            (Mode::Sync, 0),
            (Mode::Sync, MAX_WINDOW),
            (Mode::Overlapped, MAX_WINDOW),
            (Mode::Unbuffered, MAX_WINDOW),
        ];
        let mut expected = None;
        // The first round warms every mode. Later rounds alternate ordering.
        for round in 0..7 {
            let mut ordered = policies;
            if round % 2 == 0 {
                ordered.reverse();
            }
            for (mode, window) in ordered {
                let mut source = WindowsVolume::open(drive).expect("owned volume");
                source.prefetch = if mode == Mode::Sync {
                    None
                } else {
                    Some(Engine::open(&path, source.handle, mode).expect("native I/O required"))
                };
                let start = Instant::now();
                let mut reader = MftReader::open(source).expect("owned MFT");
                reader.window.limit = window;
                let entries = reader.entries();
                let scan = start.elapsed();
                assert!(reader.is_complete());
                assert!(entries.len() > 2048, "corpus must cross 1 MiB");
                let paths = paths_for(&entries);
                let map: BTreeMap<_, _> = to_stat_map(&entries, &paths, &root, false, true)
                    .into_iter()
                    .map(|(p, s)| (p, (s.logical, s.physical, s.files)))
                    .collect();
                let total = start.elapsed();
                let (effective, submitted, completed) = match reader.source.prefetch.as_ref() {
                    None => ("sync", 0, 0),
                    Some(engine) => {
                        let wanted = if mode == Mode::Unbuffered {
                            "unbuffered"
                        } else {
                            "overlapped"
                        };
                        assert_eq!(engine.effective, wanted, "fallback is not a measurement");
                        assert!(engine.submitted > 0 && engine.completed > 0);
                        assert!(engine.pending.is_none());
                        (engine.effective, engine.submitted, engine.completed)
                    }
                };
                let actual = (entries, map);
                if let Some(expected) = &expected {
                    assert_eq!(&actual, expected);
                }
                if round > 0 {
                    eprintln!("mft_pipeline {{\"round\":{round},\"io\":\"{effective}\",\"window\":{window},\"entries\":{},\"submitted\":{submitted},\"completed\":{completed},\"scan_ns\":{},\"aggregation_ns\":{},\"total_ns\":{}}}",
                        actual.0.len(), scan.as_nanos(), (total-scan).as_nanos(), total.as_nanos());
                }
                if expected.is_none() {
                    expected = Some(actual);
                }
            }
        }
    }
}
