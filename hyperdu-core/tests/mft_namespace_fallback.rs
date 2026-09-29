//! A CI-owned read-only NTFS image with a cross-folder hard link must decline
//! raw MFT attribution and retain normal enumeration semantics, not a partial map.
#![cfg(all(windows, target_env = "msvc"))]

use std::path::PathBuf;

use hyperdu_core::{scan_directory, try_scan_directory_via_mft, Options};

#[test]
fn ambiguous_namespace_declines_raw_mft_and_falls_back_without_losing_bytes() {
    if std::env::var_os("HYPERDU_MFT_NAMESPACE_FIXTURE").is_none() {
        eprintln!("skipped: requires the CI-owned cross-folder NTFS fixture");
        return;
    }
    let root = PathBuf::from(std::env::var_os("HYPERDU_MFT_PARITY_ROOT").expect("fixture root"));
    let options = Options {
        use_mft: true,
        ..Options::default()
    };
    assert!(hyperdu_core::mft_backend_applies(&root, &options));
    assert!(
        try_scan_directory_via_mft(&root, &options).is_none(),
        "record-order hard-link attribution must not be presented as enumeration parity"
    );
    let fallback = scan_directory(&root, &options).expect("automatic enumeration fallback");
    let normal = scan_directory(&root, &Options::default()).expect("explicit enumeration");
    let fixture = root.join("hyperdu-fixture");
    for map in [&normal, &fallback] {
        let stat = map.get(&fixture).expect("complete fixture total");
        assert_eq!(
            (stat.logical, stat.physical, stat.files),
            (9437184, 8388608, 130)
        );
    }
    eprintln!("cross-folder link: raw MFT declined; fallback retains exact known totals");
}
