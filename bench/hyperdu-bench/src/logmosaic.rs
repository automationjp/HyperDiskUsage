//! Real LogMosaic file-tail -> canonical adapter -> text exporter integration.
use crate::{create_private, process::OwnedChild, sha256, Event, MAX_EVENTS, MAX_JSON_BYTES};
use anyhow::{ensure, Context, Result};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

pub const CONTRACT_REV: &str = "662947a47d33ccd5a641479137b2796a9a92fb00";

pub fn read_events(path: &Path) -> Result<Vec<Event>> {
    ensure!(
        std::fs::metadata(path)?.len() <= MAX_JSON_BYTES,
        "event artifact exceeds byte limit"
    );
    let mut events = Vec::new();
    for line in BufReader::new(File::open(path)?).lines() {
        ensure!(
            events.len() < MAX_EVENTS,
            "event count limit exceeded; refusing to truncate"
        );
        let event: Event = serde_json::from_str(&line?)?;
        ensure!(
            event.attributes.is_object() && event.resource.is_object(),
            "event attributes/resource must be objects"
        );
        events.push(event);
    }
    Ok(events)
}

pub fn timeline(path: &Path, run: &str, mut events: Vec<Event>) -> Result<()> {
    ensure!(events.len() <= MAX_EVENTS, "too many events");
    let mut ids = HashSet::new();
    for e in &events {
        ensure!(
            e.trace_id == run && e.attributes["experiment.id"] == run,
            "mixed experiment IDs"
        );
        ensure!(
            ids.insert(
                e.attributes["event.id"]
                    .as_str()
                    .context("missing event ID")?
                    .to_owned()
            ),
            "duplicate event ID"
        );
        ensure!(
            e.attributes["time.monotonic_ns"].as_u64().is_some(),
            "missing monotonic time"
        );
    }
    events.sort_by_key(|e| {
        (
            e.attributes["time.monotonic_ns"].as_u64().unwrap(),
            e.attributes["event.id"].as_str().unwrap().to_owned(),
        )
    });
    let mut f = create_private(path)?;
    let mut bytes = 0u64;
    for e in events {
        let line = serde_json::to_vec(&e)?;
        bytes += line.len() as u64 + 1;
        ensure!(bytes <= MAX_JSON_BYTES, "timeline byte limit exceeded");
        f.write_all(&line)?;
        f.write_all(b"\n")?;
    }
    f.sync_all()?;
    Ok(())
}

fn quoted_path(p: &Path) -> Result<String> {
    let s = p
        .to_str()
        .context("LogMosaic config requires a Unicode path")?;
    ensure!(
        !s.contains("${"),
        "environment interpolation is not allowed in artifact paths"
    );
    Ok(serde_json::to_string(s)?)
}

/// Verify delivery, not merely process exit. Added adapter attributes are allowed.
pub fn verify_export(input: &Path, output: &Path) -> Result<usize> {
    let expected = read_events(input)?;
    let actual = read_events(output)?;
    ensure!(
        expected.len() == actual.len(),
        "LogMosaic delivery count mismatch"
    );
    let mut by_id = BTreeMap::new();
    for e in actual {
        let id = e.attributes["event.id"]
            .as_str()
            .context("export lost event ID")?
            .to_string();
        ensure!(by_id.insert(id, e).is_none(), "duplicate exported event");
    }
    for e in &expected {
        let id = e.attributes["event.id"].as_str().unwrap();
        let a = by_id.get(id).context("missing exported event")?;
        ensure!(
            a.timestamp_unix_nanos == e.timestamp_unix_nanos
                && a.trace_id == e.trace_id
                && a.span_id == e.span_id,
            "LogMosaic altered timestamp or correlation"
        );
        ensure!(
            a.body == e.body && a.resource["service.name"] == e.resource["service.name"],
            "LogMosaic altered event identity"
        );
        for (k, v) in e.attributes.as_object().unwrap() {
            ensure!(
                a.attributes.get(k) == Some(v),
                "LogMosaic altered attribute {k}"
            );
        }
    }
    Ok(expected.len())
}

pub fn ingest(agent: &Path, dir: &Path, timeout: Duration) -> Result<Value> {
    let input = dir.join("timeline.jsonl");
    let output = dir.join("logmosaic.jsonl");
    ensure!(!output.exists(), "refusing duplicate LogMosaic delivery");
    let config = format!("[programs.hyperdu_bench]\n\n[[programs.hyperdu_bench.sources]]\nkind = \"file_tail\"\npath = {}\nadapter_set = \"logmosaic_json_to_otlp\"\n\n[[programs.hyperdu_bench.exporters]]\nkind = \"text\"\nenabled = true\nformat = \"jsonl\"\npath = {}\n", quoted_path(&input)?, quoted_path(&output)?);
    let config_path = dir.join("logmosaic.toml");
    create_private(&config_path)?.write_all(config.as_bytes())?;
    let digest = sha256(agent)?;
    let status = OwnedChild::spawn(
        Command::new(agent)
            .arg("run-once")
            .arg(&config_path)
            .stdin(Stdio::null())
            .stdout(create_private(&dir.join("agent.stdout"))?)
            .stderr(create_private(&dir.join("agent.stderr"))?),
    )?
    .wait(timeout)?;
    ensure!(
        status.success(),
        "LogMosaic run-once failed; see agent.stderr"
    );
    ensure!(
        digest == sha256(agent)?,
        "LogMosaic binary changed during delivery"
    );
    let count = verify_export(&input, &output)?;
    Ok(
        json!({"status":"verified", "binary_sha256":digest, "events":count,
        "contract_tested_against":CONTRACT_REV, "delivery":"post-run; original event timestamps preserved",
        "export_sha256":sha256(&output)?}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::Clock;
    #[test]
    fn delivery_rejects_changed_timestamps_and_duplicates() {
        let tmp = tempfile::tempdir().unwrap();
        let c = Clock::capture().unwrap();
        let run = "b".repeat(32);
        let e = Event::new(&c, &run, "test", 0, c.mono_ns, 1, 1, "test", json!({})).unwrap();
        let input = tmp.path().join("in");
        let output = tmp.path().join("out");
        timeline(&input, &run, vec![e.clone()]).unwrap();
        timeline(&output, &run, vec![e.clone()]).unwrap();
        assert_eq!(verify_export(&input, &output).unwrap(), 1);
        std::fs::remove_file(&output).unwrap();
        let mut wrong = e.clone();
        wrong.timestamp_unix_nanos += 1;
        timeline(&output, &run, vec![wrong]).unwrap();
        assert!(verify_export(&input, &output).is_err());
        assert!(timeline(&tmp.path().join("dup"), &run, vec![e.clone(), e]).is_err());
    }
}
