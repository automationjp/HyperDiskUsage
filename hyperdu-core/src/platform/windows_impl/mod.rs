//! Windows backend.
//!
//! Two enumeration strategies share the same per-entry logic (`entry.rs`):
//!
//! * `nt`: `NtQueryDirectoryFile` with `FileIdBothDirectoryInformation`. One
//!   syscall returns a 64 KiB batch of entries including the allocation size
//!   and the 64-bit file id, so physical sizes and hardlink dedupe need **no**
//!   per-file syscalls. Used by default on MSVC builds.
//! * `win32`: `FindFirstFileExW` (large fetch). Portable fallback; physical
//!   sizes and file ids cost one path-based syscall per file.
//!
//! Set `HYPERDU_WIN_USE_NTQUERY=0` to force the Win32 path.

mod entry;
/// NTFS on-disk parsing for the `$MFT` backend (#15). Pure parsing, and not yet
/// wired into `process_dir`: reading a volume needs administrator rights, so
/// the parser is landed and unit-tested first.
#[cfg(target_env = "msvc")]
mod mft;
#[cfg(target_env = "msvc")]
mod mft_aggregate;
/// Reading MFT records off a volume, on top of `mft`. The volume is behind a
/// trait so the whole path is testable against a synthetic volume without the
/// administrator rights a real one needs.
#[cfg(target_env = "msvc")]
mod mft_reader;
#[cfg(target_env = "msvc")]
mod nt;
mod path;
mod win32;

use crate::{DirContext, ScanContext, StatMap};

/// Volume serial of `path`, or zero when it cannot be read.
/// Allocated size of one file. See [`crate::platform::allocated_size`].
pub fn allocated_size(path: &std::path::Path) -> Option<u64> {
    entry::allocation_size_by_path(&path::to_wide_for_open(path))
}

pub fn volume_id(path: &std::path::Path) -> u64 {
    entry::file_id_by_path(&path::to_wide_for_open(path))
        .map(|(vol, _)| vol)
        .unwrap_or(0)
}

/// Only a resolved fixed-drive root can seed the no-follow volume shortcut.
/// Remote/unknown roots retain per-directory identity queries (notably SMB DFS).
pub(crate) fn local_root_volume_for_reuse(root: &std::path::Path) -> Option<u64> {
    use windows::{core::PCWSTR, Win32::Storage::FileSystem::GetDriveTypeW};
    let resolved = root.canonicalize().ok()?;
    let drive = disk_root(&resolved)?;
    let drive_type = unsafe { GetDriveTypeW(PCWSTR(drive.as_ptr())) };
    reusable_volume(drive_type, volume_id(&resolved))
}

fn disk_root(path: &std::path::Path) -> Option<[u16; 4]> {
    use std::path::{Component, Prefix};
    match path.components().next()? {
        Component::Prefix(prefix) => match prefix.kind() {
            Prefix::Disk(drive) | Prefix::VerbatimDisk(drive) => {
                Some([u16::from(drive), b':' as u16, b'\\' as u16, 0])
            }
            _ => None,
        },
        _ => None,
    }
}

fn reusable_volume(drive_type: u32, volume: u64) -> Option<u64> {
    // DRIVE_FIXED = 3. Unknown, removable and remote media stay conservative.
    (drive_type == 3 && volume != 0).then_some(volume)
}

pub fn process_dir(ctx: &ScanContext, dctx: &DirContext, map: &mut StatMap) {
    #[cfg(target_env = "msvc")]
    {
        if nt_enabled() && !volume_rejected_nt(dctx.dir) {
            match nt::process_dir(ctx, dctx, map) {
                nt::Outcome::Done => return,
                // Support for the info class is a property of the filesystem
                // driver, so remember the volume rather than paying for a failed
                // attempt on every directory it holds.
                nt::Outcome::Unsupported => remember_nt_rejection(dctx.dir),
            }
        }
    }
    win32::process_dir(ctx, dctx, map)
}

/// Scan a whole volume by reading its `$MFT`. See
/// [`crate::platform::scan_volume_via_mft`].
///
/// Every `None` here is a reason to use the enumeration backend instead, and
/// none of them is an error worth reporting: not asked for, not elevated, not a
/// volume root, not NTFS, or the parse did not hold together.
#[cfg(target_env = "msvc")]
pub fn scan_volume_via_mft(root: &std::path::Path, opt: &crate::Options) -> Option<crate::StatMap> {
    if opt.cancel.load(std::sync::atomic::Ordering::Relaxed) {
        return None;
    }
    let drive = mft_drive(root, opt)?;
    let mut volume = mft_reader::WindowsVolume::open(drive)?;
    let mut reader = mft_reader::MftReader::open(&mut volume)?;
    let geometry = reader.geometry();
    // Narrow the read alignment now that the geometry is known, so records stop
    // pulling in more sectors than they need. Must happen before the records are
    // read, not after -- it was doing nothing where it used to be.
    reader
        .source_mut()
        .set_sector_size(geometry.bytes_per_sector);

    // Captured before the reader is dropped: the first diagnostic printed
    // record_count twice, which hid the very number that mattered.
    let diag_runs = reader.run_count();
    let diag_clusters = reader.mft_clusters();
    let mut last_progress = 0;
    let entries = reader
        .entries_with_control(|progress| report_mft_progress(opt, &mut last_progress, progress))?;
    if !reader.is_complete() {
        log::warn!("MFT DATA extents incomplete or inconsistent; declining MFT result");
        if std::env::var_os("HYPERDU_MFT_DIAG").is_some() {
            eprintln!("mft-diag: rejected: DATA extents incomplete or inconsistent; no MFT result");
        }
        return None;
    }
    let record_count = reader.record_count();
    drop(reader);
    if opt.cancel.load(std::sync::atomic::Ordering::Relaxed) {
        return None;
    }

    let prefix = format!("{}:\\", drive.to_ascii_uppercase());
    let map =
        mft_aggregate::to_stat_map(&entries, &prefix, opt.count_hardlinks, opt.compute_physical);

    if opt.cancel.load(std::sync::atomic::Ordering::Relaxed) {
        return None;
    }

    // Where records are lost between the MFT and the final map is not something
    // to guess at: the first fix for an under-count was aimed at the wrong
    // stage. Off unless asked for. See #15.
    if std::env::var_os("HYPERDU_MFT_DIAG").is_some() {
        let files: u64 = map.values().map(|s| s.files).sum();
        let dirs = entries.iter().filter(|e| e.is_directory).count();
        eprintln!(
            "mft-diag: runs={diag_runs} mft_clusters={diag_clusters} record_count={record_count} \
             entries={} (dirs={dirs}) map_dirs={} files={files}",
            entries.len(),
            map.len(),
        );

        // Where the physical total comes from, split by the categories whose
        // allocated size means different things. The MFT and the enumeration
        // API disagree by 37% on a real volume (#39); guessing which category
        // holds the difference is how the last three attempts went wrong.
        use mft::attr_flags;
        // (count, physical) for plain / compressed / sparse / encrypted
        let mut cat = [(0u64, 0u64); 4];
        for e in entries.iter().filter(|e| !e.is_directory) {
            let i = if e.data_flags & attr_flags::COMPRESSED != 0 {
                1
            } else if e.data_flags & attr_flags::SPARSE != 0 {
                2
            } else if e.data_flags & attr_flags::ENCRYPTED != 0 {
                3
            } else {
                0
            };
            cat[i].0 += 1;
            cat[i].1 = cat[i].1.saturating_add(e.sizes.allocated_size);
        }
        eprintln!(
            "mft-diag: physical by $DATA flags -- plain: {} files {} bytes | \
             compressed: {} files {} bytes | sparse: {} files {} bytes | \
             encrypted: {} files {} bytes",
            cat[0].0, cat[0].1, cat[1].0, cat[1].1, cat[2].0, cat[2].1, cat[3].0, cat[3].1,
        );

        // Allocated far above real is the signature of reading an uncompressed
        // allocation for data the volume actually stores compressed.
        let (over_n, over_bytes) = entries
            .iter()
            .filter(|e| !e.is_directory)
            .filter(|e| e.sizes.allocated_size > e.sizes.real_size.saturating_mul(2))
            .fold((0u64, 0u64), |(n, b), e| {
                (
                    n + 1,
                    b.saturating_add(e.sizes.allocated_size - e.sizes.real_size),
                )
            });
        eprintln!("mft-diag: allocated > 2x real -- {over_n} files, {over_bytes} bytes of excess");

        // The three candidate causes of the remaining 6.1 GB gap (#41), counted
        // rather than guessed at. Whichever carries the bytes is the one to fix.
        let mut stale = (0u64, 0u64);
        let mut listed = (0u64, 0u64);
        let mut named = (0u64, 0u64);
        for e in entries.iter().filter(|e| !e.is_directory) {
            let s = &e.size_source;
            if !s.from_data_attribute {
                stale.0 += 1;
                stale.1 = stale.1.saturating_add(e.sizes.allocated_size);
            }
            if s.has_attribute_list {
                listed.0 += 1;
                listed.1 = listed.1.saturating_add(e.sizes.allocated_size);
            }
            if s.named_stream_bytes > 0 {
                named.0 += 1;
                named.1 = named.1.saturating_add(s.named_stream_bytes);
            }
        }
        eprintln!(
            "mft-diag: gap candidates -- $FILE_NAME fallback: {} files {} bytes | \
             has $ATTRIBUTE_LIST: {} files {} bytes | \
             named streams: {} files {} bytes (not counted)",
            stale.0, stale.1, listed.0, listed.1, named.0, named.1
        );
    }

    Some(map)
}

#[cfg(target_env = "msvc")]
fn report_mft_progress(
    opt: &crate::Options,
    last: &mut u64,
    progress: mft_reader::ReadProgress,
) -> bool {
    use std::sync::atomic::Ordering;
    if opt.cancel.load(Ordering::Relaxed) {
        return false;
    }
    if let (Some(bucket), Some(previous)) = (
        progress.files.checked_div(opt.progress_every),
        last.checked_div(opt.progress_every),
    ) {
        if let Some(callback) = &opt.progress_callback {
            if progress.records == 0 || progress.finished || bucket > previous {
                callback(progress.files);
                *last = progress.files;
            }
        }
    }
    // A callback may itself request cancellation. No fabricated path is sent
    // to progress_sample_callback: MFT names have not been resolved here.
    !opt.cancel.load(Ordering::Relaxed)
}

#[cfg(all(test, target_env = "msvc"))]
mod mft_progress_tests {
    use std::sync::{atomic::Ordering, Arc, Mutex};

    use super::*;

    #[test]
    fn progress_threshold_initial_final_and_disabled_callbacks() {
        let values = Arc::new(Mutex::new(Vec::new()));
        let seen = values.clone();
        let opt = crate::Options {
            progress_every: 512,
            progress_callback: Some(Arc::new(move |n| seen.lock().unwrap().push(n))),
            progress_sample_callback: Some(Arc::new(|_| panic!("no synthetic path samples"))),
            ..crate::Options::default()
        };
        let mut last = 0;
        for (records, files, finished) in [
            (0, 0, false),
            (256, 200, false),
            (512, 400, false),
            (768, 600, false),
            (800, 620, true),
        ] {
            assert!(report_mft_progress(
                &opt,
                &mut last,
                mft_reader::ReadProgress {
                    records,
                    files,
                    finished
                }
            ));
        }
        assert_eq!(*values.lock().unwrap(), [0, 600, 620]);
        let disabled = crate::Options {
            progress_every: 0,
            ..opt
        };
        assert!(report_mft_progress(
            &disabled,
            &mut last,
            mft_reader::ReadProgress {
                records: 0,
                files: 0,
                finished: false
            }
        ));
        assert_eq!(*values.lock().unwrap(), [0, 600, 620]);
    }

    #[test]
    fn callback_cancellation_is_observed_before_the_next_read() {
        let opt = crate::Options::default();
        let cancel = opt.cancel.clone();
        let opt = crate::Options {
            progress_every: 1,
            progress_callback: Some(Arc::new(move |_| cancel.store(true, Ordering::Relaxed))),
            ..opt
        };
        assert!(!report_mft_progress(
            &opt,
            &mut 0,
            mft_reader::ReadProgress {
                records: 0,
                files: 0,
                finished: false
            }
        ));
    }
}

#[cfg(not(target_env = "msvc"))]
pub fn scan_volume_via_mft(
    _root: &std::path::Path,
    _opt: &crate::Options,
) -> Option<crate::StatMap> {
    None
}

/// Check only eligibility, not whether opening and parsing the volume succeeds.
/// See [`crate::mft_backend_applies`] and [`crate::try_scan_directory_via_mft`].
#[cfg(target_env = "msvc")]
pub fn mft_backend_applies(root: &std::path::Path, opt: &crate::Options) -> bool {
    mft_drive(root, opt).is_some()
}

#[cfg(not(target_env = "msvc"))]
pub fn mft_backend_applies(_root: &std::path::Path, _opt: &crate::Options) -> bool {
    false
}

/// Drive letter to read the MFT of, or `None` when the backend does not apply.
///
/// The single place the preconditions live: asked for, a volume root, and
/// elevated, with supported enumeration options. Opening the volume can still fail afterwards (not NTFS, or the
/// parse does not hold), which the caller also treats as "use enumeration".
#[cfg(target_env = "msvc")]
fn mft_drive(root: &std::path::Path, opt: &crate::Options) -> Option<char> {
    // The direct reader cannot preserve these enumeration semantics. Decline
    // before opening the volume so callers retain the requested filters.
    if !opt.use_mft
        || !opt.exclude_contains.is_empty()
        || !opt.exclude_regex.is_empty()
        || !opt.exclude_glob.is_empty()
        || opt.exclude_ac.is_some()
        || opt.exclude_regex_set.is_some()
        || opt.exclude_glob_set.is_some()
        || !opt.exclude_contains_w.is_empty()
        || opt.max_depth != 0
        || opt.min_file_size != 0
        || opt.follow_links
        || opt.count_hardlinks
        || opt.approximate_sizes
        || opt.inode_cache.is_some()
    {
        return None;
    }
    // The MFT covers a whole volume. Scanning a subdirectory this way would
    // mean reading every record and discarding most of them, which is slower
    // than walking the subdirectory -- and the point of this backend is that it
    // does not walk.
    let drive = volume_root_letter(root)?;
    if !mft_reader::is_elevated() {
        return None;
    }
    Some(drive)
}

/// Drive letter when `root` is the root of a volume (`C:\`), else `None`.
///
/// A subdirectory returns `None` on purpose: see `scan_volume_via_mft`.
#[cfg(target_env = "msvc")]
fn volume_root_letter(root: &std::path::Path) -> Option<char> {
    use std::path::{Component, Prefix};

    let mut components = root.components();
    let letter = match components.next()? {
        Component::Prefix(p) => match p.kind() {
            Prefix::Disk(d) | Prefix::VerbatimDisk(d) => d as char,
            // UNC shares and device paths have no MFT we can open this way.
            _ => return None,
        },
        _ => return None,
    };
    // After the prefix there must be a root and nothing else.
    match components.next() {
        Some(Component::RootDir) => {}
        _ => return None,
    }
    if components.next().is_some() {
        return None;
    }
    Some(letter)
}

/// Prefix identifying the volume of `dir` (`C:`, `\\?\C:`, `\\server\share`).
/// Relative paths have none and are simply never cached.
#[cfg(target_env = "msvc")]
fn volume_of(dir: &std::path::Path) -> Option<std::ffi::OsString> {
    match dir.components().next() {
        Some(std::path::Component::Prefix(p)) => Some(p.as_os_str().to_os_string()),
        _ => None,
    }
}

/// Volumes whose driver rejected `FileIdFullDirectoryInformation`.
#[cfg(target_env = "msvc")]
mod rejected {
    use std::{
        ffi::OsString,
        sync::{
            atomic::{AtomicBool, Ordering},
            Mutex, OnceLock,
        },
    };

    /// Rejection is rare, so the per-directory check stays a single relaxed load
    /// until the first one happens.
    pub(super) static ANY: AtomicBool = AtomicBool::new(false);

    pub(super) fn list() -> &'static Mutex<Vec<OsString>> {
        static LIST: OnceLock<Mutex<Vec<OsString>>> = OnceLock::new();
        LIST.get_or_init(|| Mutex::new(Vec::new()))
    }

    pub(super) fn mark() {
        ANY.store(true, Ordering::Relaxed);
    }
}

#[cfg(target_env = "msvc")]
fn volume_rejected_nt(dir: &std::path::Path) -> bool {
    use std::sync::atomic::Ordering;
    if !rejected::ANY.load(Ordering::Relaxed) {
        return false;
    }
    let Some(vol) = volume_of(dir) else {
        return false;
    };
    let guard = rejected::list().lock().unwrap_or_else(|e| e.into_inner());
    guard.contains(&vol)
}

#[cfg(target_env = "msvc")]
fn remember_nt_rejection(dir: &std::path::Path) {
    let Some(vol) = volume_of(dir) else {
        return;
    };
    let mut guard = rejected::list().lock().unwrap_or_else(|e| e.into_inner());
    if !guard.contains(&vol) {
        guard.push(vol);
    }
    rejected::mark();
}

/// Read the opt-out environment variable once per process (not per directory).
#[cfg(target_env = "msvc")]
fn nt_enabled() -> bool {
    use std::sync::OnceLock;
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("HYPERDU_WIN_USE_NTQUERY")
            .map(|v| v != "0" && !v.eq_ignore_ascii_case("false"))
            .unwrap_or(true)
    })
}

#[cfg(test)]
mod volume_reuse_tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn only_known_fixed_volumes_are_reusable() {
        assert_eq!(reusable_volume(3, 7), Some(7));
        assert_eq!(reusable_volume(3, 0), None);
        for kind in [0, 1, 2, 4, 5, 6] {
            assert_eq!(reusable_volume(kind, 7), None);
        }
    }

    #[test]
    fn unc_and_unknown_roots_cannot_seed_a_local_volume() {
        assert!(disk_root(Path::new(r"C:\data")).is_some());
        assert!(disk_root(Path::new(r"\\?\C:\data")).is_some());
        assert!(disk_root(Path::new(r"\\server\share\data")).is_none());
        assert!(disk_root(Path::new(r"\\?\UNC\server\share")).is_none());
        assert!(disk_root(Path::new("relative")).is_none());
    }
}
