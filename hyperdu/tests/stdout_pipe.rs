//! Result output must survive a reader that goes away, and the file exports
//! must not inherit the listing's `--top` limit.
use std::{
    fs,
    io::{BufRead, BufReader},
    path::Path,
    process::{Command, Stdio},
};

/// Enough directories that the listing cannot fit in a pipe buffer, so the
/// child is still writing once the reader is gone.
const PIPE_FILLING_DIRS: usize = 4000;

fn make_dirs(root: &Path, n: usize) {
    for i in 0..n {
        fs::create_dir(root.join(format!("d{i:05}"))).unwrap();
    }
}

#[test]
fn closed_stdout_pipe_ends_quietly_and_still_writes_the_export() {
    let scan = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    make_dirs(scan.path(), PIPE_FILLING_DIRS);
    let csv = out.path().join("report.csv");

    let mut child = Command::new(env!("CARGO_BIN_EXE_hyperdu"))
        .arg(scan.path())
        .args(["--top", "100000"])
        .arg("--csv")
        .arg(&csv)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    // Read one line, then drop the pipe: from here the child writes into a
    // reader that no longer exists, which is what `hyperdu … | head` does.
    {
        let mut reader = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        assert!(line.starts_with("Top "), "{line}");
    }

    let finished = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&finished.stderr).into_owned();
    assert!(
        !stderr.contains("panicked"),
        "a closed stdout must not panic: {stderr}"
    );
    assert!(
        finished.status.success(),
        "status={:?} stderr={stderr}",
        finished.status
    );
    assert!(
        csv.is_file(),
        "the requested export must still be written: {stderr}"
    );
}

#[test]
fn csv_export_keeps_the_rows_below_the_top_limit() {
    let scan = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    make_dirs(scan.path(), 5);
    let csv = out.path().join("report.csv");

    let finished = Command::new(env!("CARGO_BIN_EXE_hyperdu"))
        .arg(scan.path())
        .args(["--top", "2"])
        .arg("--csv")
        .arg(&csv)
        .output()
        .unwrap();
    assert!(finished.status.success(), "{finished:?}");

    // The root plus its five children, none of them dropped by `--top 2`.
    let text = fs::read_to_string(&csv).unwrap();
    assert_eq!(text.lines().count() - 1, 6, "{text}");

    // The listing itself still honours `--top`. Only its ranked lines start
    // with a number; the `Total:` summary carries the same `phys=` column.
    let stdout = String::from_utf8(finished.stdout).unwrap();
    let ranked = stdout
        .lines()
        .filter(|l| l.trim_start().starts_with(|c: char| c.is_ascii_digit()))
        .count();
    assert_eq!(ranked, 2, "{stdout}");
}
