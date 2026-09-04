use crate::{DirContext, ScanContext, StatMap};

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub mod linux_helpers;
#[cfg(all(
    target_os = "linux",
    target_arch = "x86_64",
    feature = "uring",
    not(target_env = "musl")
))]
mod linux_uring_impl;
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

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub fn process_dir_wrapped(ctx: &ScanContext, dir_ctx: &DirContext, map: &mut StatMap) {
    // Prefer io_uring by default when compiled and supported; otherwise fallback to getdents64
    #[cfg(all(feature = "uring", not(target_env = "musl")))]
    {
        // Runtime guard: allow disabling uring via options or env.
        // The io_uring backend discovers subdirectories from d_type alone, so it
        // has no inode to test for symlink cycles; the getdents64 backend does.
        // Following links therefore has to use the latter.
        let disable = ctx.options.disable_uring
            || ctx.options.follow_links
            || std::env::var("HYPERDU_DISABLE_URING").ok().as_deref() == Some("1")
            || std::env::var("HYPERDU_DISABLE_URING")
                .ok()
                .map(|v| v.eq_ignore_ascii_case("true"))
                .unwrap_or(false);
        if disable {
            linux_x86_64_impl::process_dir(ctx, dir_ctx, map);
        } else {
            linux_uring_impl::process_dir(ctx, dir_ctx, map);
        }
    }
    #[cfg(any(not(feature = "uring"), target_env = "musl"))]
    {
        linux_x86_64_impl::process_dir(ctx, dir_ctx, map);
    }
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
