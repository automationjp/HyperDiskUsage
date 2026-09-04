use std::process::Command;

fn bin_path() -> String {
    // Cargo sets CARGO_BIN_EXE_<name> for integration tests
    if let Ok(p) = std::env::var("CARGO_BIN_EXE_hyperdu-cli") {
        return p;
    }
    if let Ok(p) = std::env::var("CARGO_BIN_EXE_hyperdu_cli") {
        return p;
    }
    let target = std::env::var("CARGO_TARGET_DIR").unwrap_or_else(|_| "target".into());
    format!("{target}/debug/hyperdu-cli")
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

    let exe = bin_path();
    if std::fs::metadata(&exe).is_err() {
        eprintln!("skip: test binary not found at {exe}");
        return;
    }
    let out = Command::new(exe)
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
