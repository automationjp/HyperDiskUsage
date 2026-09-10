#![cfg(any(windows, target_os = "linux", target_os = "macos"))]

use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

fn run(action: &str, root: &Path, db: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_hyperdu"))
        .args(["index", action])
        .arg(root)
        .arg("--database")
        .arg(db)
        .output()
        .unwrap()
}

fn json(out: Output) -> serde_json::Value {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

#[test]
fn snapshot_reads_cached_totals_until_explicit_refresh() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("root");
    let db = temp.path().join("index.bin");
    fs::create_dir_all(root.join("child")).unwrap();
    fs::write(root.join("child/file"), vec![1u8; 4096]).unwrap();
    let first = json(run("refresh", &root, &db));
    assert_eq!(first["files"], 1);
    assert_eq!(first["freshness"], "stale");
    fs::write(root.join("another"), vec![1u8; 8192]).unwrap();
    let cached = json(run("show", &root, &db));
    assert_eq!(cached["physical_bytes"], first["physical_bytes"]);
    assert_eq!(cached["files"], 1);
    let refreshed = json(run("refresh", &root, &db));
    assert_eq!(refreshed["files"], 2);
    assert!(
        refreshed["physical_bytes"].as_u64().unwrap() > first["physical_bytes"].as_u64().unwrap()
    );
}

#[test]
fn refuses_an_unrelated_root_and_self_indexing() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("root");
    let other = temp.path().join("other");
    let db = temp.path().join("index.bin");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&other).unwrap();
    json(run("refresh", &root, &db));
    assert!(!run("show", &other, &db).status.success());
    assert!(!run("refresh", &root, &root.join("self.bin"))
        .status
        .success());
    let previous = fs::read(&db).unwrap();
    assert!(!run("refresh", &root.join("missing"), &db).status.success());
    assert_eq!(fs::read(&db).unwrap(), previous);
}

#[test]
fn regular_scan_flags_are_not_silently_accepted_by_snapshot_commands() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("root");
    let db = temp.path().join("index.bin");
    fs::create_dir(&root).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_hyperdu"))
        .args(["--apparent-size", "index", "refresh"])
        .arg(&root)
        .arg("--database")
        .arg(&db)
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(!db.exists());
}

#[test]
fn a_directory_named_index_can_still_be_scanned() {
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir(temp.path().join("index")).unwrap();
    for args in [vec!["./index"], vec!["--", "index"]] {
        let result = Command::new(env!("CARGO_BIN_EXE_hyperdu"))
            .current_dir(temp.path())
            .args(args)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn watch_once_reports_advisory_observation_and_saved_show_remains_stale() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("root");
    let db = temp.path().join("index.bin");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("a"), [0; 71]).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_hyperdu"))
        .args(["index", "watch"])
        .arg(&root)
        .arg("--database")
        .arg(&db)
        .arg("--once")
        .output()
        .unwrap();
    let watched = json(output);
    assert_eq!(watched["freshness"], "observed");
    assert_eq!(watched["files"], 1);
    assert_eq!(watched["logical_bytes"], 71);
    assert_eq!(watched["checkpoint_written"], true);
    let cached = json(run("show", &root, &db));
    assert_eq!(cached["freshness"], "stale");
    assert_eq!(cached["monitoring"], false);
    assert_eq!(cached["files"], 1);
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn foreground_watch_rejects_a_second_writer_and_updates_without_rescanning() {
    use std::{
        io::{BufRead, BufReader},
        process::Stdio,
        sync::mpsc,
        thread,
        time::{Duration, Instant},
    };
    struct ChildGuard(std::process::Child);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("root");
    let db = temp.path().join("index.bin");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("a"), [0; 71]).unwrap();
    let mut child = ChildGuard(
        Command::new(env!("CARGO_BIN_EXE_hyperdu"))
            .args(["index", "watch"])
            .arg(&root)
            .arg("--database")
            .arg(&db)
            .args(["--poll-ms", "20", "--checkpoint-seconds", "1"])
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    let output = child.0.stdout.take().unwrap();
    let (send, recv) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(output).lines() {
            if send.send(line).is_err() {
                break;
            }
        }
    });
    let first: serde_json::Value = serde_json::from_str(
        &recv
            .recv_timeout(Duration::from_secs(30))
            .expect("watch initial output")
            .unwrap(),
    )
    .unwrap();
    assert_eq!(first["freshness"], "observed");
    assert!(!run("refresh", &root, &db).status.success());
    fs::write(root.join("a"), [0; 8193]).unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let line = recv
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .expect("watch mutation output")
            .unwrap();
        let update: serde_json::Value = serde_json::from_str(&line).unwrap();
        if update["logical_bytes"] == 8193 {
            assert_eq!(update["freshness"], "observed");
            assert_eq!(update["rebuilt"], false);
            assert_eq!(update["directories_read"], 0);
            break;
        }
    }
    drop(child);
    json(run("refresh", &root, &db)); // OS lock released even on process termination.
}
