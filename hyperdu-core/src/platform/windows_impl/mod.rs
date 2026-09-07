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
pub fn volume_id(path: &std::path::Path) -> u64 {
    entry::file_id_by_path(&path::to_wide_for_open(path))
        .map(|(vol, _)| vol)
        .unwrap_or(0)
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
    let entries = reader.entries();
    let record_count = reader.record_count();
    drop(reader);

    let paths = mft_reader::paths_for(&entries);
    let prefix = format!("{}:\\", drive.to_ascii_uppercase());
    let map = mft_reader::to_stat_map(
        &entries,
        &paths,
        &prefix,
        opt.count_hardlinks,
        opt.compute_physical,
    );

    // Where records are lost between the MFT and the final map is not something
    // to guess at: the first fix for an under-count was aimed at the wrong
    // stage. Off unless asked for. See #15.
    if std::env::var_os("HYPERDU_MFT_DIAG").is_some() {
        let files: u64 = map.values().map(|s| s.files).sum();
        let dirs = entries.iter().filter(|e| e.is_directory).count();
        eprintln!(
            "mft-diag: runs={diag_runs} mft_clusters={diag_clusters} record_count={record_count} \
             entries={} (dirs={dirs}) paths={} map_dirs={} files={files}",
            entries.len(),
            paths.len(),
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
        // Named streams split by whether the unnamed $DATA is empty. Counting
        // all 16.4 GB of named streams would overshoot a 6.1 GB gap by ten, so
        // enumeration cannot be counting all of them. The suspected dividing
        // line is WOF compression (Compact OS): those files park their real
        // contents in a `WofCompressedData` stream and leave the unnamed $DATA
        // empty, and Windows reports the compressed bytes as the file's size.
        // An ordinary alternate data stream sits beside a non-empty $DATA and
        // is a different case. If the first bucket lands near 6.1 GB the split
        // is the answer; if it does not, this rules the theory out cheaply.
        let mut wof_like = (0u64, 0u64);
        let mut ads_like = (0u64, 0u64);
        let mut file_bytes: u64 = 0;
        for e in entries.iter().filter(|e| !e.is_directory) {
            let s = &e.size_source;
            file_bytes = file_bytes.saturating_add(e.sizes.allocated_size);
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

                // Only meaningful when the size really came from $DATA; a
                // $FILE_NAME fallback says nothing about the unnamed stream.
                let bucket = if s.from_data_attribute && e.sizes.allocated_size == 0 {
                    &mut wof_like
                } else {
                    &mut ads_like
                };
                bucket.0 += 1;
                bucket.1 = bucket.1.saturating_add(s.named_stream_bytes);
            }
        }
        eprintln!(
            "mft-diag: gap candidates -- $FILE_NAME fallback: {} files {} bytes | \
             has $ATTRIBUTE_LIST: {} files {} bytes | \
             named streams: {} files {} bytes (not counted)",
            stale.0, stale.1, listed.0, listed.1, named.0, named.1
        );
        eprintln!(
            "mft-diag: named streams split -- empty unnamed $DATA (WOF-like): {} files {} bytes | \
             non-empty unnamed $DATA (ordinary ADS): {} files {} bytes",
            wof_like.0, wof_like.1, ads_like.0, ads_like.1
        );
        // Printed together so one CI run answers "which total matches
        // enumeration" without arithmetic across three log lines.
        eprintln!(
            "mft-diag: file bytes -- as reported: {} | plus WOF-like: {} | plus all named: {}",
            file_bytes,
            file_bytes.saturating_add(wof_like.1),
            file_bytes.saturating_add(named.1)
        );
    }

    Some(map)
}

#[cfg(not(target_env = "msvc"))]
pub fn scan_volume_via_mft(
    _root: &std::path::Path,
    _opt: &crate::Options,
) -> Option<crate::StatMap> {
    None
}

/// Whether the MFT backend would be used for this root. See
/// [`crate::mft_backend_applies`].
///
/// Shares its preconditions with `scan_volume_via_mft` through `mft_drive`, so
/// the two cannot disagree -- a caller told "the MFT path will be used" and
/// then silently given enumeration would draw the wrong conclusion from a
/// comparison of the two.
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
/// elevated. Opening the volume can still fail afterwards (not NTFS, or the
/// parse does not hold), which the caller also treats as "use enumeration".
#[cfg(target_env = "msvc")]
fn mft_drive(root: &std::path::Path, opt: &crate::Options) -> Option<char> {
    if !opt.use_mft {
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
