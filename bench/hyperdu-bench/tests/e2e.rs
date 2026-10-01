#![cfg(feature = "driver")]
use std::fs;
use std::path::Path;
use std::process::Command;
use hyperdu_bench::{clock::Clock,dataset,logmosaic,read_json,sha256,write_json};

const BENCH:&str = env!("CARGO_BIN_EXE_hyperdu-bench");
const DRIVER:&str = env!("CARGO_BIN_EXE_hyperdu-bench-driver");

#[test]
fn driver_matches_independent_oracle() {
    let tmp = tempfile::tempdir().unwrap(); let root = tmp.path().join("dataset");
    let o = dataset::generate(&root,"wide",103,19).unwrap();
    let result = tmp.path().join("driver.json");
    assert!(Command::new(DRIVER).arg("--root").arg(&root).arg("--result").arg(&result)
        .args(["--threads","2"]).status().unwrap().success());
    let v = read_json(&result).unwrap();
    assert_eq!(v["files"],o.files); assert_eq!(v["logical"],o.logical);
    assert_eq!(v["directories"],o.directories); assert_eq!(v["errors"],0);
}

#[test]
fn benchmark_balances_and_retains_every_sample() {
    let tmp = tempfile::tempdir().unwrap(); let root = tmp.path().join("dataset");
    dataset::generate(&root,"wide",12,7).unwrap();
    let out = tmp.path().join("result");
    let mut cmd = Command::new(BENCH);
    cmd.arg("run").arg("--root").arg(&root).arg("--driver").arg(DRIVER)
        .arg("--base-driver").arg(DRIVER).arg("--output").arg(&out)
        .args(["--runs","4","--threads","1"]);
    assert!(cmd.status().unwrap().success());
    let v = read_json(&out.join("result.json")).unwrap();
    assert_eq!(v["complete"],true);
    assert_eq!(v["samples"].as_array().unwrap().len(),8);
    assert_eq!(v["schedule"][0],serde_json::json!([0,1]));
    assert_eq!(v["schedule"][1],serde_json::json!([1,0]));
    assert_eq!(fs::read_to_string(out.join("samples.jsonl")).unwrap().lines().count(),8);
    let digest = sha256(&out.join("result.json")).unwrap();
    assert!(!cmd.status().unwrap().success());
    assert_eq!(digest,sha256(&out.join("result.json")).unwrap());
}

#[test]
fn output_inside_dataset_is_rejected_without_mutation() {
    let tmp = tempfile::tempdir().unwrap(); let root = tmp.path().join("dataset");
    let before = dataset::generate(&root,"flat",3,7).unwrap();
    assert!(!Command::new(BENCH).arg("run").arg("--root").arg(&root)
        .arg("--driver").arg(DRIVER).arg("--output").arg(root.join("result"))
        .status().unwrap().success());
    assert_eq!(before,dataset::oracle(&root).unwrap());
}

#[test]
fn internal_and_allocation_events_share_the_clock_without_file_names() {
    let tmp = tempfile::tempdir().unwrap(); let root = tmp.path().join("private-corpus");
    dataset::generate(&root,"wide",200,13).unwrap();
    let clock = Clock::capture().unwrap(); let clock_path = tmp.path().join("clock.json");
    write_json(&clock_path,&clock).unwrap();
    let events = tmp.path().join("internal.jsonl");
    let run = "d".repeat(32);
    assert!(Command::new(DRIVER).arg("--root").arg(&root).arg("--result").arg(tmp.path().join("result.json"))
        .arg("--events").arg(&events).arg("--clock").arg(clock_path)
        .arg("--run-id").arg(&run).args(["--threads","2"]).status().unwrap().success());
    let parsed = logmosaic::read_events(&events).unwrap();
    assert!(parsed.iter().any(|e|e.body=="hyperdu.scan"));
    assert!(parsed.iter().any(|e|e.body=="hyperdu.directory"));
    assert!(parsed.iter().any(|e|e.body=="hyperdu.allocation"));
    for e in parsed {
        assert_eq!(e.trace_id,run);
        assert_eq!(e.timestamp_unix_nanos,clock.to_unix(e.attributes["time.monotonic_ns"].as_u64().unwrap()).unwrap());
    }
    assert!(!fs::read_to_string(events).unwrap().contains("private-corpus"));
}

#[cfg(any(target_os="linux",windows))]
#[test]
fn native_process_metrics_are_real_numbers() {
    let metrics = hyperdu_bench::process::snapshot(std::process::id()).unwrap();
    assert!(metrics["cpu.user_ns"].as_u64().is_some());
    assert!(metrics["cpu.system_ns"].as_u64().is_some());
    assert!(metrics["memory.rss_bytes"].as_u64().is_some());
}

#[cfg(unix)]
#[test]
fn timeout_terminates_the_owned_process() {
    use std::time::Duration;
    let mut child = hyperdu_bench::process::OwnedChild::spawn(Command::new("sh").args(["-c","sleep 30"])).unwrap();
    assert!(child.wait(Duration::from_millis(20)).is_err());
    assert!(child.0.try_wait().unwrap().is_some());
}

/// This is intentionally not a mock: CI supplies the pinned upstream executable.
#[test]
#[ignore = "requires a separately built, real LogMosaic agent"]
fn real_logmosaic_roundtrip() {
    let agent = std::env::var_os("LOGMOSAIC_TEST_AGENT").expect("LOGMOSAIC_TEST_AGENT must point to the real agent");
    assert!(Path::new(&agent).is_file());
    let tmp = tempfile::tempdir().unwrap(); let root = tmp.path().join("dataset");
    dataset::generate(&root,"wide",1000,19).unwrap();
    let out = tmp.path().join("diagnosis");
    assert!(Command::new(BENCH).arg("diagnose").arg("--root").arg(&root)
        .arg("--driver").arg(DRIVER).arg("--output").arg(&out)
        .arg("--logmosaic-agent").arg(agent).args(["--threads","2"])
        .status().unwrap().success());
    let v = read_json(&out.join("result.json")).unwrap();
    assert_eq!(v["complete"],true); assert_eq!(v["workload_executions"],1);
    assert_eq!(v["logmosaic"]["status"],"verified");
    assert_eq!(v["sources"]["perf"]["status"],"unavailable");
    assert!(logmosaic::verify_export(&out.join("timeline.jsonl"),&out.join("logmosaic.jsonl")).unwrap()>0);
}
