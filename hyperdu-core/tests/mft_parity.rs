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

/// The scan's totals, read from the root entry.
///
/// NOT `map.values().sum()`. `scan_directory` rolls totals up, so every
/// directory holds its whole subtree and summing counts each file once per
/// ancestor -- which is exactly the mistake this test made when it first ran,
/// producing a 9x "discrepancy" that was entirely the summing. Every other
/// test in this crate reads the root entry; so does this one now.
fn totals(map: &hyperdu_core::StatMap, root: &std::path::Path) -> (u64, u64, u64) {
    map.get(root)
        .map(|s| (s.logical, s.physical, s.files))
        .unwrap_or((0, 0, 0))
}

/// This found two real bugs, and neither was the one it first appeared to.
///
/// On a GitHub Actions Windows runner -- which is elevated, so it actually ran
/// -- the backends read 9x apart on files while agreeing on directories to
/// within 0.1%. Two rounds of chasing that as an MFT under-count changed
/// nothing, because neither backend was miscounting:
///
///   1. This test summed `map.values()`. `scan_directory` rolls totals up, so
///      every directory holds its subtree and the sum counts each file once per
///      ancestor. The 9x was the average tree depth. Fixed by reading the root
///      entry, as every other test in this crate already did.
///   2. `scan_directory` returned the MFT map without rolling it up, so the two
///      backends really did produce different-meaning maps -- just not in a way
///      that made either one's counts wrong. Fixed in lib.rs.
///
/// The lesson worth keeping: a 9x gap that looks like a counting bug can be a
/// units bug. `fsutil fsinfo ntfsinfo` is the tiebreaker -- an MFT of N bytes
/// holds N/1024 records, and a volume cannot have more files than records.
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

    let (wl, wp, wf) = totals(&walked, &root);
    let (ml, mp, mf) = totals(&from_mft, &root);

    eprintln!("root:        {}", root.display());
    eprintln!(
        "enumeration: logical={wl} physical={wp} files={wf} dirs={}",
        walked.len()
    );
    eprintln!(
        "mft:         logical={ml} physical={mp} files={mf} dirs={}",
        from_mft.len()
    );

    // Which side is wrong is not obvious from the file counts alone. If both
    // sides see the same directories but wildly different file counts, the
    // difference is inside directories -- hardlinks, alternate streams. If the
    // directory counts differ too, one side is walking a different tree.
    let dir_drift = if walked.is_empty() {
        0.0
    } else {
        ((from_mft.len() as f64 - walked.len() as f64) / walked.len() as f64 * 100.0).abs()
    };
    eprintln!("dir drift:   {dir_drift:.2}%");

    // The two sides disagree by 9x on files while agreeing on directories,
    // which is not a difference either one can be assumed right about. Windows
    // reports the MFT's own size, so ask it rather than arguing: an MFT of N
    // bytes holds N/1024 records, and a volume cannot have more files than
    // records.
    if let Some(letter) = root.to_string_lossy().chars().next() {
        if let Ok(out) = std::process::Command::new("fsutil")
            .args(["fsinfo", "ntfsinfo", &format!("{letter}:")])
            .output()
        {
            let text = String::from_utf8_lossy(&out.stdout);
            for line in text.lines() {
                let l = line.to_ascii_lowercase();
                if l.contains("mft") || l.contains("bytes per cluster") {
                    eprintln!("fsutil:      {}", line.trim());
                }
            }
        }
    }

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
    let phys_drift = drift(wp, mp);
    eprintln!(
        "drift:       files={file_drift:.2}% logical={byte_drift:.2}% physical={phys_drift:.2}%"
    );
    // Physical is asserted now that #39 is fixed. It was 33% out because sparse
    // attributes were charged their whole run range -- holes included -- from
    // offset 0x28, instead of the space actually spent at 0x40. That was 38.6 GB
    // across 116,283 files on this very volume, so a regression here would show
    // up immediately.
    assert!(
        phys_drift < 5.0,
        "physical totals differ by {phys_drift:.2}% (enumeration {wp}, mft {mp}). \
         Check the mft-diag breakdown above: if the sparse category dominates, \
         `parse_data_sizes` is reading 0x28 rather than 0x40 again. See #39."
    );

    assert!(
        file_drift < 5.0,
        "file counts differ by {file_drift:.2}% (enumeration {wf}, mft {mf}). \
         A live volume drifts, but not by this much. Compare the fsutil line \
         above: an MFT of N bytes holds N/1024 records, and a volume cannot \
         have more files than records. Whichever side exceeds that is the \
         wrong one -- as of #37 it is enumeration."
    );
    assert!(
        byte_drift < 5.0,
        "logical totals differ by {byte_drift:.2}% (enumeration {wl}, mft {ml}). \
         If the file counts also differ, fix that first: the byte totals \
         cannot agree while the two sides are counting different sets."
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
