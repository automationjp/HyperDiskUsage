//! The explicit backend API must not turn an unavailable MFT into a successful walk.
use hyperdu_core::{scan_directory, try_scan_directory_via_mft, Options};

#[test]
fn declining_mft_does_not_return_the_enumeration_result() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("file"), vec![1u8; 4096]).unwrap();
    let opt = Options {
        use_mft: true,
        ..Options::default()
    };
    assert!(try_scan_directory_via_mft(dir.path(), &opt).is_none());
    // The existing public scanner must still fall back for the same request.
    let map = scan_directory(dir.path(), &opt).unwrap();
    let total = map.get(dir.path()).unwrap();
    assert_eq!((total.logical, total.files), (4096, 1));
}

#[test]
fn disabled_mft_does_not_return_a_successful_empty_result() {
    let root = if cfg!(windows) { r"C:\" } else { "/" };
    assert!(try_scan_directory_via_mft(root, &Options::default()).is_none());
}
