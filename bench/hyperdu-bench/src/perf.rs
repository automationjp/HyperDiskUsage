//! One perf session wraps the measured driver; no serial profiler reruns.
//! Per-task sched events are not a system-wide scheduler/critical-path trace.
use crate::{clock::Clock, create_private, process::OwnedChild, Event, MAX_EVENTS, MAX_JSON_BYTES};
use anyhow::{ensure, Result};
use serde_json::{json, Value};
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

pub struct Perf {
    pub binary: PathBuf,
    pub events: Vec<String>,
    pub capability: Value,
}

impl Perf {
    pub fn probe(binary: &Path, dir: &Path) -> Result<Self> {
        let mut supported = Vec::new();
        let mut states = serde_json::Map::new();
        for (i, event) in [
            "cpu-clock",
            "raw_syscalls:sys_enter",
            "raw_syscalls:sys_exit",
            "sched:sched_switch",
        ]
        .iter()
        .enumerate()
        {
            let mut cmd = Command::new(binary);
            cmd.args(["stat", "-e", event, "--"])
                .arg(std::env::current_exe()?)
                .arg("probe")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(create_private(&dir.join(format!("perf-probe-{i}.stderr")))?);
            let outcome =
                OwnedChild::spawn(&mut cmd).and_then(|mut c| c.wait(Duration::from_secs(5)));
            match outcome {
                Ok(s) if s.success() => {
                    supported.push(event.to_string());
                    states.insert(event.to_string(), json!({"status":"available"}));
                }
                other => {
                    states.insert(event.to_string(),json!({"status":"unavailable","reason":format!("{other:?}"),"evidence":format!("perf-probe-{i}.stderr")}));
                }
            }
        }
        Ok(Self {
            binary: binary.to_path_buf(),
            events: supported,
            capability: Value::Object(states),
        })
    }
    pub fn command(&self, driver: &Path, dir: &Path) -> Command {
        let mut c = Command::new(&self.binary);
        c.args(["record", "--clockid", "mono", "-o"])
            .arg(dir.join("perf.data"));
        for event in &self.events {
            // CPU stacks are sampled; syscall/scheduler records do not copy user stacks.
            let configured = if event == "cpu-clock" {
                "cpu-clock/freq=99,call-graph=dwarf/".to_string()
            } else {
                // Tracepoints default to every event; their grammar rejects period=1.
                format!("{event}/call-graph=no/")
            };
            c.arg("-e").arg(configured);
        }
        c.arg("--").arg(driver);
        c
    }
    pub fn decode(
        &self,
        dir: &Path,
        clock: &Clock,
        run: &str,
        target: u32,
    ) -> Result<(Vec<Event>, Value)> {
        ensure!(
            fs::metadata(dir.join("perf.data"))?.len() <= MAX_JSON_BYTES,
            "perf artifact quota exceeded"
        );
        let text = dir.join("perf-script.txt");
        let status = OwnedChild::spawn(
            Command::new(&self.binary)
                .args(["script", "--ns", "-i"])
                .arg(dir.join("perf.data"))
                .args([
                    "-F",
                    "sw:pid,tid,time,event,ip,sym",
                    "-F",
                    "trace:pid,tid,time,event,trace",
                ])
                .stdin(Stdio::null())
                .stdout(create_private(&text)?)
                .stderr(create_private(&dir.join("perf-script.stderr"))?),
        )?
        .wait(Duration::from_secs(60))?;
        ensure!(status.success(), "perf script failed");
        ensure!(
            fs::metadata(&text)?.len() <= MAX_JSON_BYTES,
            "perf text too large"
        );
        let mut out = Vec::new();
        let mut unparsed = 0usize;
        let mut filtered = 0usize;
        for line in BufReader::new(File::open(text)?).lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            match parse_header(&line) {
                Some((pid, tid, mono, name, detail)) if pid == target => {
                    ensure!(out.len() < MAX_EVENTS, "perf event limit exceeded");
                    let category = if name.starts_with("raw_syscalls:") {
                        "syscall"
                    } else if name.starts_with("sched:") {
                        "scheduler"
                    } else {
                        "cpu"
                    };
                    out.push(Event::new(clock,run,"perf",out.len() as u64,mono,pid,tid,"perf.event",
                        json!({"profile.category":category,"perf.event":name,"perf.detail":detail,
                               "measurement.kind":"sample-or-tracepoint; not wall-time attribution"}))?);
                }
                Some(_) => {
                    filtered += 1;
                }
                None => {
                    // Call-chain continuation lines start with an address, not a sample header.
                    if !line
                        .split_whitespace()
                        .next()
                        .is_some_and(|s| s.len() >= 8 && s.bytes().all(|b| b.is_ascii_hexdigit()))
                    {
                        unparsed += 1;
                    }
                }
            }
        }
        let stderr = fs::read_to_string(dir.join("target.stderr")).unwrap_or_default();
        let lost = stderr.to_ascii_lowercase().contains("lost");
        let status = if out.is_empty() {
            "no_samples"
        } else if lost || unparsed > 0 {
            "partial"
        } else {
            "collected"
        };
        Ok((
            out,
            json!({"status":status,"unparsed_lines":unparsed,"other_process_samples":filtered,
                      "loss_diagnostic_seen":lost,"capabilities":self.capability,
                      "scope":"target process and its sampled threads; no inferred critical path"}),
        ))
    }
}

fn seconds_ns(text: &str) -> Option<u64> {
    let (s, f) = text.trim_end_matches(':').split_once('.')?;
    if f.len() > 9 || f.is_empty() || !f.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    s.parse::<u64>()
        .ok()?
        .checked_mul(1_000_000_000)?
        .checked_add(f.parse::<u64>().ok()? * 10u64.pow(9 - f.len() as u32))
}

pub fn parse_header(line: &str) -> Option<(u32, u64, u64, String, String)> {
    let mut t = line.split_whitespace();
    let first = t.next()?;
    let (pid, tid) = if let Some((p, t)) = first.split_once('/') {
        (p.parse().ok()?, t.parse().ok()?)
    } else {
        (first.parse().ok()?, t.next()?.parse().ok()?)
    };
    let mono = seconds_ns(t.next()?)?;
    let name = t.next()?.trim_end_matches(':').to_string();
    if name.is_empty() {
        return None;
    }
    let detail = t.collect::<Vec<_>>().join(" ");
    // Raw artifact remains local; normalized event records are bounded.
    let detail = detail.chars().take(2048).collect();
    Some((pid, tid, mono, name, detail))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_fixed_perf_fields_without_float_time_rounding() {
        let row = parse_header(" 21/22 1234567.000000123: raw_syscalls:sys_enter: NR 332 (0, 1)")
            .unwrap();
        assert_eq!((row.0, row.1, row.2), (21, 22, 1234567000000123));
        assert_eq!(row.3, "raw_syscalls:sys_enter");
        assert!(parse_header("garbage").is_none());
        assert_eq!(seconds_ns("1.1234567890:"), None);
        assert_eq!(
            parse_header("21 22 3.100: cpu-clock: 7fff statx")
                .unwrap()
                .2,
            3100000000
        );
    }
}

#[cfg(test)]
mod command_tests {
    use super::*;
    #[test]
    fn sampling_periods_are_per_event() {
        let p = Perf {
            binary: "perf".into(),
            events: vec!["cpu-clock".into(), "raw_syscalls:sys_enter".into()],
            capability: json!({}),
        };
        let c = p.command(Path::new("driver"), Path::new("out"));
        let args: Vec<_> = c
            .get_args()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        assert!(args.contains(&"cpu-clock/freq=99,call-graph=dwarf/".into()));
        assert!(args.contains(&"raw_syscalls:sys_enter/call-graph=no/".into()));
        assert!(!args.iter().any(|a| a.contains("period=1")));
        assert!(!args.contains(&"-F".into()));
        assert!(!args.contains(&"-c".into()));
        assert!(!args.contains(&"--call-graph".into()));
    }
}
