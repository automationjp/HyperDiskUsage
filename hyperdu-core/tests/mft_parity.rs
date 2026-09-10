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

use hyperdu_core::{scan_directory, try_scan_directory_via_mft, Options};

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
    let total = map.get(root).expect("scan must contain the requested root");
    (total.logical, total.physical, total.files)
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

    // Skip only an ordinary local invocation without the required privileges.
    // CI explicitly provides a freshly created NTFS volume: ineligibility is
    // then a failure, not a passing test that performed no comparison.
    if !hyperdu_core::mft_backend_applies(&root, &mft_opt) {
        assert!(
            std::env::var_os("HYPERDU_MFT_PARITY_ROOT").is_none(),
            "the explicitly requested parity volume must be eligible for MFT: {}",
            root.display()
        );
        eprintln!(
            "skipped: the MFT backend does not apply to {}.\n\
             \x20        Run from an elevated shell against a volume root (C:\\),\n\
             \x20        or set HYPERDU_MFT_PARITY_ROOT. See #15.",
            root.display()
        );
        return;
    }

    // Eligibility is not proof of backend execution. Obtain MFT first so a
    // declined parse fails immediately, without an expensive comparison of
    // enumeration against another enumeration. No process-global env mutation.
    let from_mft = try_scan_directory_via_mft(&root, &mft_opt)
        .expect("MFT did not complete; enumeration fallback is not a parity result");
    eprintln!("parity backend: mft (no fallback)");
    let walked_opt = Options {
        use_mft: false,
        ..Options::default()
    };
    let walked = scan_directory(&root, &walked_opt).expect("enumeration scan");

    if std::env::var_os("HYPERDU_MFT_PARITY_FIXTURE").is_some() {
        assert_fixture(&from_mft, &root);
        assert_fixture(&walked, &root);
    }

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
    // Retain the historical live-volume tolerance. A controlled CI fixture
    // must additionally meet exact known totals below; a low live drift is not
    // sufficient evidence that the MFT path executed (Refs #41, #47).
    const PHYSICAL_BAR: f64 = 8.0;
    assert!(
        phys_drift < PHYSICAL_BAR,
        "physical totals differ by {phys_drift:.2}% (enumeration {wp}, mft {mp}), \
         above the {PHYSICAL_BAR}% live-volume tolerance. Check the mft-diag breakdown above: if the sparse \
         category is back in the tens of gigabytes, `parse_data_sizes` is \
         charging holes again. See #39."
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

/// Contract for scripts/dev/ntfs_fixture.ps1. Both backends must independently
/// match known fixture totals, not merely agree on the same erroneous number.
fn assert_fixture(map: &hyperdu_core::StatMap, root: &std::path::Path) {
    let fixture = root.join("hyperdu-fixture");
    assert_eq!(
        totals(map, &fixture.join("plain")),
        (128 * 65536, 128 * 65536, 128)
    );
    assert_eq!(totals(map, &fixture.join("sparse")), (1048576, 0, 2));
    assert_eq!(
        totals(map, &fixture),
        (128 * 65536 + 1048576, 128 * 65536, 130)
    );
    assert!(map.contains_key(&fixture.join("empty")));
    let total = totals(map, root);
    let children = map
        .iter()
        .filter(|(path, _)| path.parent() == Some(root))
        .fold((0, 0, 0), |(l, p, f), (_, s)| {
            (l + s.logical, p + s.physical, f + s.files)
        });
    assert_eq!(
        (
            total.0 - children.0,
            total.1 - children.1,
            total.2 - children.2
        ),
        (65536, 65536, 1)
    );
}

/// Unsupported MFT options must select enumeration, never silently lose a filter.
/// CI sets the owned fixture root; an ordinary unelevated local run is not evidence.
#[test]
fn the_mft_backend_declines_unsupported_options() {
    let root = parity_root();
    let plain = Options {
        use_mft: true,
        threads: 1,
        ..Options::default()
    };
    if std::env::var_os("HYPERDU_MFT_PARITY_FIXTURE").is_none() {
        eprintln!("skipped: unsupported-option parity needs the owned NTFS fixture");
        return;
    }
    assert!(
        hyperdu_core::mft_backend_applies(&root, &plain),
        "fixture must permit MFT without filters"
    );
    let options = [
        Options {
            exclude_contains: vec!["plain".into()],
            ..plain.clone()
        },
        Options {
            exclude_glob: vec!["**/plain/**".into()],
            ..plain.clone()
        },
        Options {
            exclude_regex: vec!["plain".into()],
            ..plain.clone()
        },
        Options {
            min_file_size: 1 << 30,
            ..plain.clone()
        },
        Options {
            max_depth: 1,
            ..plain.clone()
        },
        Options {
            follow_links: true,
            ..plain.clone()
        },
        Options {
            count_hardlinks: true,
            ..plain.clone()
        },
        Options {
            approximate_sizes: true,
            compute_physical: false,
            ..plain.clone()
        },
    ];
    fn rows(map: hyperdu_core::StatMap) -> Vec<(PathBuf, u64, u64, u64)> {
        let mut rows: Vec<_> = map
            .into_iter()
            .map(|(p, s)| (p, s.logical, s.physical, s.files))
            .collect();
        rows.sort();
        rows
    }
    for (case, opt) in options.into_iter().enumerate() {
        assert!(
            !hyperdu_core::mft_backend_applies(&root, &opt),
            "unsupported case {case} was eligible"
        );
        assert!(
            try_scan_directory_via_mft(&root, &opt).is_none(),
            "unsupported case {case} used MFT"
        );
        let expected = scan_directory(
            &root,
            &Options {
                use_mft: false,
                ..opt.clone()
            },
        )
        .unwrap();
        let actual = scan_directory(&root, &opt).unwrap();
        assert_eq!(
            rows(actual),
            rows(expected),
            "fallback differs for case {case}"
        );
    }
}
