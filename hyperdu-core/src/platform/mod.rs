use crate::{DirContext, ScanContext, StatMap};

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub mod linux_helpers;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
mod linux_x86_64_impl;
#[cfg(target_os = "macos")]
mod macos_impl;
#[cfg(all(
    unix,
    not(target_os = "macos"),
    not(all(target_os = "linux", target_arch = "x86_64"))
))]
mod unix_fallback_impl;
#[cfg(windows)]
mod windows_impl;

/// Identifier of the filesystem holding `path`, in the same form the
/// per-directory check compares against. Zero when it cannot be determined,
/// which backends read as "no boundary known" so that `--one-file-system`
/// fails open instead of silently scanning nothing.
#[cfg(unix)]
pub fn filesystem_id(path: &std::path::Path) -> u64 {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    let Ok(c_path) = CString::new(path.as_os_str().as_bytes()) else {
        return 0;
    };
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::stat(c_path.as_ptr(), &mut st) } != 0 {
        return 0;
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        linux_helpers::packed_dev(st.st_dev)
    }
    #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
    {
        st.st_dev as u64
    }
}

#[cfg(windows)]
pub fn filesystem_id(path: &std::path::Path) -> u64 {
    windows_impl::volume_id(path)
}

#[cfg(windows)]
pub fn process_dir_wrapped(ctx: &ScanContext, dir_ctx: &DirContext, map: &mut StatMap) {
    windows_impl::process_dir(ctx, dir_ctx, map)
}

/// Scan a whole volume by reading its `$MFT`, or `None` when that is not
/// possible here.
///
/// Unlike the enumeration backends this is not per-directory: the MFT is read
/// once for the whole volume. `None` covers every reason it might not apply --
/// not Windows, not elevated, not NTFS, not a volume root -- and the caller
/// falls back to enumeration. Reporting a partial answer would be worse than
/// being slower.
#[cfg(windows)]
pub fn scan_volume_via_mft(root: &std::path::Path, opt: &crate::Options) -> Option<StatMap> {
    windows_impl::scan_volume_via_mft(root, opt)
}

#[cfg(not(windows))]
pub fn scan_volume_via_mft(_root: &std::path::Path, _opt: &crate::Options) -> Option<StatMap> {
    None
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub fn process_dir_wrapped(ctx: &ScanContext, dir_ctx: &DirContext, map: &mut StatMap) {
    linux_x86_64_impl::process_dir(ctx, dir_ctx, map);
}

#[cfg(all(
    unix,
    not(target_os = "macos"),
    not(all(target_os = "linux", target_arch = "x86_64"))
))]
pub fn process_dir_wrapped(ctx: &ScanContext, dir_ctx: &DirContext, map: &mut StatMap) {
    unix_fallback_impl::process_dir(ctx, dir_ctx, map)
}

#[cfg(target_os = "macos")]
pub fn process_dir_wrapped(ctx: &ScanContext, dir_ctx: &DirContext, map: &mut StatMap) {
    macos_impl::process_dir(ctx, dir_ctx, map)
}
