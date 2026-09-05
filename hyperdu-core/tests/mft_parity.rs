//! Does the `$MFT` backend agree with directory enumeration? (#15)
//!
//! This is the check that decides whether the MFT backend is correct. It cannot
//! run without administrator rights -- `\\.\C:` is not openable otherwise -- so
//! it skips rather than fails on an ordinary machine, and prints why.
//!
//! Everything else about the backend is covered by unit tests over a synthetic
//! volume. What is left, and what only a real volume can answer, is whether
//! reading the MFT and walking the directories arrive at the same totals.
//!
//! To run it:
//!
//! ```text
//! # from an elevated shell
//! cargo test -p hyperdu-core --test mft_parity -- --nocapture
//! ```
//!
//! Set `HYPERDU_MFT_PARITY_ROOT` to test a volume other than `C:\`.

#![cfg(all(windows, target_env = "msvc"))]

use std::path::PathBuf;

use hyperdu_core::{scan_directory, Options};

/// Volume to compare on. A whole volume is required: the MFT backend declines
/// anything else, which would make the test compare enumeration with itself.
fn parity_root() -> PathBuf {
    std::env::var("HYPERDU_MFT_PARITY_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(r"C:\"))
}

fn totals(map: &hyperdu_core::StatMap) -> (u64, u64, u64) {
    map.values().fold((0, 0, 0), |(l, p, f), s| {
        (l + s.logical, p + s.physical, f + s.files)
    })
}

/// This found a real bug once, and is kept running so it can again.
///
/// On a GitHub Actions Windows runner -- which is elevated, so it actually ran
/// -- the MFT backend reported 1,134,722 files where enumeration found
/// 10,220,623: it was missing 88.9% of them. `$MFT` is a file, and on a volume
/// with ten million records it is fragmented enough that its own `$DATA` runs
/// spill into extension records reachable only through `$ATTRIBUTE_LIST`.
/// `MftReader::open` read the base record's run list and stopped, seeing a
/// fraction of the MFT while still reporting a plausible total.
///
/// The reader now follows `$ATTRIBUTE_LIST`; whether that is enough is what
/// this test answers.
#[test]
fn the_mft_backend_agrees_with_directory_enumeration() {
    let root = parity_root();

    let mft_opt = Options {
        use_mft: true,
        ..Options::default()
    };

    // Distinguishing "the totals match because both used enumeration" from "the
    // totals match because the two backends agree" is the whole point, so check
    // the backend actually engages before comparing anything.
    if !hyperdu_core::mft_backend_applies(&root, &mft_opt) {
        eprintln!(
            "skipped: the MFT backend does not apply to {}.\n\
             \x20        Run from an elevated shell against a volume root (C:\\),\n\
             \x20        or set HYPERDU_MFT_PARITY_ROOT. See #15.",
            root.display()
        );
        return;
    }

    let walked_opt = Options {
        use_mft: false,
        ..Options::default()
    };
    let walked = scan_directory(&root, &walked_opt).expect("enumeration scan");

    // A failure here needs to say *where* the records went. The first attempt
    // at fixing an under-count was aimed at the wrong stage because the only
    // number available was the final one.
    //
    // SAFETY: single-threaded test setup, before any scan starts.
    unsafe { std::env::set_var("HYPERDU_MFT_DIAG", "1") };
    let from_mft = scan_directory(&root, &mft_opt).expect("mft scan");
    unsafe { std::env::remove_var("HYPERDU_MFT_DIAG") };

    let (wl, wp, wf) = totals(&walked);
    let (ml, mp, mf) = totals(&from_mft);

    eprintln!("root:        {}", root.display());
    eprintln!("enumeration: logical={wl} physical={wp} files={wf}");
    eprintln!("mft:         logical={ml} physical={mp} files={mf}");

    // A live volume changes under the scan, so an exact match is not the bar.
    // A backend that is structurally wrong -- missing extension records,
    // double-counting 8.3 aliases, charging sparse holes -- is wrong by far
    // more than a system volume drifts in the seconds between two scans.
    let drift = |a: u64, b: u64| -> f64 {
        if a == 0 {
            return if b == 0 { 0.0 } else { 100.0 };
        }
        ((b as f64 - a as f64) / a as f64 * 100.0).abs()
    };

    let file_drift = drift(wf, mf);
    let byte_drift = drift(wl, ml);
    eprintln!("drift:       files={file_drift:.2}% logical={byte_drift:.2}%");

    assert!(
        file_drift < 5.0,
        "file counts differ by {file_drift:.2}% (enumeration {wf}, mft {mf}). \
         A live volume drifts, but not by this much -- suspect extension \
         records or 8.3 aliases."
    );
    assert!(
        byte_drift < 5.0,
        "logical totals differ by {byte_drift:.2}% (enumeration {wl}, mft {ml}). \
         Suspect $ATTRIBUTE_LIST handling, sparse runs, or resident $DATA."
    );
}

#[test]
fn the_mft_backend_declines_a_subdirectory() {
    // Reading every record to report one directory would be slower than walking
    // it, so the backend must decline. If it stopped declining, the parity test
    // above would silently compare enumeration against itself.
    let opt = Options {
        use_mft: true,
        ..Options::default()
    };
    let sub = parity_root().join("Windows");
    assert!(
        !hyperdu_core::mft_backend_applies(&sub, &opt),
        "the MFT backend must only apply to a volume root"
    );
}

#[test]
fn use_mft_off_never_engages_the_backend() {
    let opt = Options::default(); // use_mft defaults to false
    assert!(
        !hyperdu_core::mft_backend_applies(parity_root(), &opt),
        "the backend must stay off unless asked for"
    );
}
