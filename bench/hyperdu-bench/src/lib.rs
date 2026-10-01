//! Benchmark orchestration is independent of the scanner used by the driver.
//! LogMosaic remains an external, separately licensed collector, not vendored code.
pub mod analysis;
pub mod clock;
pub mod dataset;
pub mod logmosaic;
pub mod perf;
pub mod process;

use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;

pub const MAX_EVENTS: usize = 200_000;
pub const MAX_JSON_BYTES: u64 = 128 * 1024 * 1024;

pub fn sha256(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    let mut buf = [0; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

pub fn create_private(path: &Path) -> Result<File> {
    let mut opt = OpenOptions::new();
    opt.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opt.mode(0o600);
    }
    Ok(opt.open(path)?)
}

pub fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut f = create_private(path)?;
    serde_json::to_writer_pretty(&mut f, value)?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    Ok(())
}

pub fn read_json(path: &Path) -> Result<Value> {
    ensure!(
        fs::metadata(path)?.len() <= MAX_JSON_BYTES,
        "JSON exceeds size limit"
    );
    Ok(serde_json::from_reader(File::open(path)?)?)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub timestamp_unix_nanos: u64,
    pub severity_text: String,
    pub severity_number: u8,
    pub body: String,
    pub trace_id: String,
    pub span_id: String,
    pub resource: Value,
    pub attributes: Value,
}

impl Event {
    // The canonical record explicitly names each correlation field.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        clock: &clock::Clock,
        run: &str,
        source: &str,
        seq: u64,
        mono: u64,
        pid: u32,
        tid: u64,
        body: &str,
        mut attrs: Value,
    ) -> Result<Self> {
        ensure!(
            run.len() == 32 && run.bytes().all(|b| b.is_ascii_hexdigit()),
            "invalid run ID"
        );
        let map = attrs
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("attributes must be an object"))?;
        map.insert("experiment.id".into(), json!(run));
        map.insert(
            "event.id".into(),
            json!(format!("{run}:{source}:{pid}:{seq}")),
        );
        map.insert("profile.source".into(), json!(source));
        map.insert("process.pid".into(), json!(pid));
        map.insert("thread.id".into(), json!(tid));
        map.insert("time.monotonic_ns".into(), json!(mono));
        map.insert("time.clock".into(), json!(clock.domain));
        let digest = Sha256::digest(format!("{run}:{source}:{pid}:{seq}").as_bytes());
        Ok(Self {
            timestamp_unix_nanos: clock.to_unix(mono)?,
            severity_text: "INFO".into(),
            severity_number: 9,
            body: body.into(),
            trace_id: run.into(),
            span_id: format!("{:x}", digest)[..16].to_string(),
            resource: json!({"service.name":"hyperdu-bench"}),
            attributes: attrs,
        })
    }
}

pub fn median(values: &[u64]) -> Option<u64> {
    if values.is_empty() {
        return None;
    }
    let mut v = values.to_vec();
    v.sort_unstable();
    let m = v.len() / 2;
    Some(if v.len() % 2 == 1 {
        v[m]
    } else {
        v[m - 1] + (v[m] - v[m - 1]) / 2
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn medians_do_not_overflow() {
        assert_eq!(median(&[]), None);
        assert_eq!(median(&[9, 1, 5]), Some(5));
        assert_eq!(median(&[u64::MAX, u64::MAX]), Some(u64::MAX));
    }
    #[test]
    fn event_ids_and_timestamps_are_explicit() {
        let c = clock::Clock {
            mono_ns: 100,
            unix_ns: 1000,
            uncertainty_ns: 2,
            domain: "test".into(),
        };
        let e = Event::new(
            &c,
            &"a".repeat(32),
            "process",
            1,
            110,
            20,
            21,
            "sample",
            json!({}),
        )
        .unwrap();
        assert_eq!(e.timestamp_unix_nanos, 1010);
        assert_eq!(e.attributes["time.monotonic_ns"], 110);
        assert_eq!(e.span_id.len(), 16);
    }
}
