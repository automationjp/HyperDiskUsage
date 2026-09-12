//! Embeds the application icon into the Windows executable.
//!
//! Only a Windows host building a Windows target does anything here. The
//! `winresource` build-dependency sits under
//! `[target.'cfg(windows)'.build-dependencies]`, and Cargo matches that cfg
//! against the *host*, so on Linux -- including a mingw cross-build to
//! windows-gnu -- this file compiles to a no-op and no resource compiler is
//! needed. A missing or failing compiler only costs the icon: the build prints
//! a warning and continues, so `cargo install` never breaks over it.

fn main() {
    println!("cargo:rerun-if-changed=assets/hyperdu.ico");
    println!("cargo:rerun-if-env-changed=HYPERDU_SKIP_ICON");
    #[cfg(windows)]
    windows::embed_icon();
}

#[cfg(windows)]
mod windows {
    use std::env;

    const ICON: &str = "assets/hyperdu.ico";

    pub fn embed_icon() {
        if env::var("CARGO_CFG_TARGET_OS").ok().as_deref() != Some("windows") {
            return;
        }
        if matches!(env::var("HYPERDU_SKIP_ICON"), Ok(v) if !v.is_empty() && v != "0") {
            println!("cargo:warning=HYPERDU_SKIP_ICON is set; building without the icon");
            return;
        }
        let mut resource = winresource::WindowsResource::new();
        resource.set_icon(ICON);
        if let Err(err) = resource.compile() {
            println!(
                "cargo:warning=could not embed {ICON} into the executable ({err}); \
                 the binary works without it"
            );
        }
    }
}
