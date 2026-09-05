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
    // Narrow the read alignment now that the geometry is known, so records stop
    // pulling in more sectors than they need.
    let mut reader = mft_reader::MftReader::open(&mut volume)?;
    let geometry = reader.geometry();
    let entries = reader.entries();
    drop(reader);
    volume.set_sector_size(geometry.bytes_per_sector);

    let paths = mft_reader::paths_for(&entries);
    let prefix = format!("{}:\\", drive.to_ascii_uppercase());
    Some(mft_reader::to_stat_map(
        &entries,
        &paths,
        &prefix,
        opt.count_hardlinks,
        opt.compute_physical,
    ))
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
