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
        if nt_enabled() && nt::process_dir(ctx, dctx, map) == nt::Outcome::Done {
            return;
        }
    }
    win32::process_dir(ctx, dctx, map)
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
