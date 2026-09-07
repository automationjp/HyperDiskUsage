use std::process::Command;

/// The binary under test.
///
/// `env!`, not `std::env::var`: cargo sets `CARGO_BIN_EXE_<name>` at compile
/// time for integration tests, so a wrong name is a build failure rather than a
/// runtime miss. The previous version guessed at three paths and returned early
/// when none of them existed, which meant renaming the binary made this test
/// pass without ever starting the CLI. A test that skips itself when its
/// subject is missing reports the same "ok" as one that ran.
fn bin_path() -> &'static str {
    env!("CARGO_BIN_EXE_hyperdu")
}

/// GNU du 互換出力: `-b`（= --apparent-size --block-size=1）でディレクトリ行のみ、
/// `サイズ<TAB>パス` 形式。ファイル行は `-a` を付けない限り出力しない。
#[test]
fn du_tab_output_lists_directories_only() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("r");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("b.txt"), vec![0u8; 1024]).unwrap();
    std::fs::write(root.join("a.txt"), vec![0u8; 2048]).unwrap();

    let out = Command::new(bin_path())
        .arg(&root)
        .arg("--compat")
        .arg("gnu")
        .arg("-b")
        .output()
        .unwrap();
    assert!(out.status.success(), "cli failed: status={:?}", out.status);
    let s = String::from_utf8_lossy(&out.stdout);

    let lines: Vec<&str> = s.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(lines.len(), 1, "one directory line expected, got: {s:?}");
    let (size, path) = lines[0]
        .split_once('\t')
        .unwrap_or_else(|| panic!("tab separated `size<TAB>path`, got: {:?}", lines[0]));
    assert_eq!(size, "3072", "apparent size in bytes");
    assert_eq!(std::path::Path::new(path), root.as_path());
    assert!(!s.contains("a.txt"), "files are not listed without -a");
}
