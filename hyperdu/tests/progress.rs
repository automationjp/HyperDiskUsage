//! Progress is diagnostic output and must not delay a completed scan.
use std::{
    process::Command,
    time::{Duration, Instant},
};

#[test]
fn progress_uses_stderr_and_completion_wakes_the_status_thread() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("file.bin"), [1u8; 512]).unwrap();
    let started = Instant::now();
    let output = Command::new(env!("CARGO_BIN_EXE_hyperdu"))
        .arg(dir.path())
        .args(["--progress", "--progress-every", "1", "--top", "1"])
        .env("HYPERDU_PROGRESS_KEEPALIVE_SECS", "60")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("scanning"), "{stderr}");
    assert!(stderr.contains("progress:"), "{stderr}");
    assert!(
        !stdout.contains("progress:") && !stdout.contains("sample:"),
        "{stdout}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "completion must wake a 60-second status wait"
    );
}
