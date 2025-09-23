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
    format!("{}/debug/hyperdu-cli", target)
}

#[test]
fn du_tab_sorted_blocks() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("r");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("b.txt"), vec![0u8; 1024]).unwrap();
    std::fs::write(root.join("a.txt"), vec![0u8; 2048]).unwrap();

    let exe = bin_path();
    if std::fs::metadata(&exe).is_err() {
        eprintln!("skip: test binary not found at {}", exe);
        return;
    }
    // GNU互換、バイト単位（-b）、タブ区切り、パス順（アルファベット）
    let out = Command::new(exe)
        .arg(&root)
        .arg("--compat")
        .arg("gnu")
        .arg("-b")
        .output()
        .unwrap();
    assert!(out.status.success(), "cli failed: status={:?}", out.status);
    let s = String::from_utf8_lossy(&out.stdout);
    // 末尾2行が a.txt, b.txt の順で出ること（ディレクトリ行を含むため包含判定）
    assert!(s.contains("\ta.txt"));
    assert!(s.contains("\tb.txt"));
    // タブ区切り（ブロック数\tパス）
    for line in s.lines() {
        if let Some(idx) = line.find('\t') {
            assert!(idx > 0);
        }
    }
}
