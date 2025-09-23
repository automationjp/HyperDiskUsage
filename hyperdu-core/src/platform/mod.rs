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

#[cfg(windows)]
pub fn process_dir_wrapped(ctx: &ScanContext, dir_ctx: &DirContext, map: &mut StatMap) {
    windows_impl::process_dir(ctx, dir_ctx, map)
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub fn process_dir_wrapped(ctx: &ScanContext, dir_ctx: &DirContext, map: &mut StatMap) {
    // Prefer io_uring by default when compiled and supported; otherwise fallback to getdents64
    #[cfg(all(feature = "uring", not(target_env = "musl")))]
    {
        // Runtime guard: allow disabling uring via options or env
        let disable = ctx.options.disable_uring
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
