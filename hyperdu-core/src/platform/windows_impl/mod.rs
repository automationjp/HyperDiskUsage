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
#[cfg(target_env = "msvc")]
mod nt;
mod path;
mod win32;

use crate::{DirContext, ScanContext, StatMap};

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
