//! Summarize records delivered by LogMosaic, not a separate profiling rerun.
//! CPU samples and overlapping thread intervals are never presented as additive wall time.
use crate::{create_private, logmosaic, write_json, Event};
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;

fn mono(e: &Event) -> u64 {
    e.attributes["time.monotonic_ns"].as_u64().unwrap_or(0)
}
fn delta(first: Option<&Event>, last: Option<&Event>, key: &str) -> Option<u64> {
    last?.attributes[key]
        .as_u64()?
        .checked_sub(first?.attributes[key].as_u64()?)
}
fn syscall_id(detail: &str) -> Option<String> {
    let nr = detail.find("NR ")?;
    let digits: String = detail[nr + 3..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    (!digits.is_empty()).then_some(digits)
}

pub fn summarize(events: &[Event]) -> Value {
    let mut sources = BTreeMap::<String, u64>::new();
    let mut workers = BTreeMap::<u64, (u64, u64, u64)>::new();
    let mut cpu = BTreeMap::<String, u64>::new();
    let mut calls = BTreeMap::<String, (u64, u64, u64, u64)>::new();
    let mut open = BTreeMap::<(u64, u64), (String, u64)>::new();
    let mut unmatched = 0u64;
    let start = events.iter().find(|e| e.body == "process.scan_start");
    let end = events.iter().rev().find(|e| e.body == "process.scan_end");
    for e in events {
        *sources
            .entry(
                e.attributes["profile.source"]
                    .as_str()
                    .unwrap_or("unknown")
                    .into(),
            )
            .or_default() += 1;
        let tid = e.attributes["thread.id"].as_u64().unwrap_or(0);
        let pid = e.attributes["process.pid"].as_u64().unwrap_or(0);
        if e.body == "hyperdu.directory" {
            let duration = e.attributes["duration_ns"].as_u64().unwrap_or(0);
            let w = workers.entry(tid).or_default();
            w.0 += 1;
            w.1 = w.1.saturating_add(duration);
            w.2 = w.2.max(duration);
        }
        let name = e.attributes["perf.event"].as_str().unwrap_or("");
        let detail = e.attributes["perf.detail"].as_str().unwrap_or("");
        if name.starts_with("cpu-clock") {
            let symbol = detail.split_once(' ').map_or(detail, |(_, s)| s).to_owned();
            *cpu.entry(symbol).or_default() += 1;
        }
        if name.starts_with("raw_syscalls:sys_enter") {
            if let Some(id) = syscall_id(detail) {
                calls.entry(id.clone()).or_default().0 += 1;
                if open.insert((pid, tid), (id, mono(e))).is_some() {
                    unmatched += 1;
                }
            } else {
                unmatched += 1;
            }
        } else if name.starts_with("raw_syscalls:sys_exit") {
            match (open.remove(&(pid, tid)), syscall_id(detail)) {
                (Some((id, begin)), Some(exit)) if id == exit && mono(e) >= begin => {
                    let duration = mono(e) - begin;
                    let c = calls.entry(id).or_default();
                    c.1 += 1;
                    c.2 = c.2.saturating_add(duration);
                    c.3 = c.3.max(duration);
                }
                _ => {
                    unmatched += 1;
                }
            }
        }
    }
    unmatched += open.len() as u64;
    let mut hotspots: Vec<_> = cpu.into_iter().collect();
    hotspots.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let cpu_total: u64 = hotspots.iter().map(|(_, n)| *n).sum();
    let scan = events
        .iter()
        .find(|e| e.body == "hyperdu.scan")
        .and_then(|e| e.attributes["duration_ns"].as_u64());
    let allocation = events
        .iter()
        .rev()
        .find(|e| e.body == "hyperdu.allocation")
        .map(|e| e.attributes.clone());
    json!({"event_count":events.len(),"source_event_counts":sources,"scan_wall_ns":scan,
        "process_scan_delta":{"user_cpu_ns":delta(start,end,"cpu.user_ns"),
            "system_cpu_ns":delta(start,end,"cpu.system_ns"),
            "physical_read_bytes":delta(start,end,"io.read_bytes"),
            "physical_write_bytes":delta(start,end,"io.write_bytes"),
            "logical_read_bytes":delta(start,end,"io.logical_read_bytes"),
            "logical_write_bytes":delta(start,end,"io.logical_write_bytes")},
        "cpu":{"status":if cpu_total>0 {"sampled"} else {"no_samples"},"samples":cpu_total,
            "hotspots":hotspots.into_iter().take(20).map(|(symbol,n)|json!({"symbol":symbol,"samples":n,
                "share_of_cpu_samples":n as f64 / cpu_total.max(1) as f64})).collect::<Vec<_>>(),
            "interpretation":"sample distribution only; not percentage of wall time; consult loss/symbol coverage"},
        "syscalls":{"unmatched_records":unmatched,"by_number":calls.into_iter().map(|(nr,c)|json!({
            "number":nr,"entries":c.0,"matched_pairs":c.1,"summed_thread_elapsed_ns":c.2,"max_elapsed_ns":c.3})).collect::<Vec<_>>(),
            "interpretation":"entry/exit elapsed intervals include running and blocked time; numbers are architecture-specific; not disk-only latency"},
        "workers":workers.into_iter().map(|(tid,w)|json!({"tid":tid,"directory_jobs":w.0,
            "summed_directory_thread_elapsed_ns":w.1,"longest_directory_ns":w.2})).collect::<Vec<_>>(),
        "worker_interpretation":"directory jobs may overlap; summed worker intervals are not total wall time or scheduler idle time",
        "allocation":allocation,"causal_conclusion":"not inferred"})
}

pub fn trace_events(events: &[Event]) -> Value {
    let origin = events.iter().map(mono).min().unwrap_or(0);
    let records: Vec<_> = events
        .iter()
        .map(|e| {
            let mut v = json!({"name":e.body,"cat":e.attributes["profile.category"],
            "pid":e.attributes["process.pid"],"tid":e.attributes["thread.id"],
            "ts":mono(e).saturating_sub(origin) as f64 / 1000.0,"args":e.attributes});
            if let Some(duration) = e.attributes["duration_ns"].as_u64() {
                v["ph"] = json!("X");
                v["dur"] = json!(duration as f64 / 1000.0);
            } else if e.body == "process.sample" || e.body == "hyperdu.allocation" {
                v["ph"] = json!("C");
                v["args"] = Value::Object(
                    e.attributes
                        .as_object()
                        .unwrap()
                        .iter()
                        .filter(|(k, value)| {
                            value.is_number()
                                && !matches!(
                                    k.as_str(),
                                    "time.monotonic_ns" | "process.pid" | "thread.id"
                                )
                        })
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect(),
                );
            } else {
                v["ph"] = json!("i");
                v["s"] = json!("t");
            }
            v
        })
        .collect();
    json!({"traceEvents":records,"displayTimeUnit":"ms","otherData":{
        "clock_origin_monotonic_ns":origin,"origin":"LogMosaic verified exported records",
        "note":"trace timestamps use microseconds; exact nanoseconds are retained in logmosaic.jsonl"}})
}

pub fn generate(dir: &Path) -> Result<Value> {
    let mut events = logmosaic::read_events(&dir.join("logmosaic.jsonl"))?;
    events.sort_by_key(mono);
    let summary = summarize(&events);
    write_json(&dir.join("bottlenecks.json"), &summary)?;
    write_json(&dir.join("timeline.trace.json"), &trace_events(&events))?;
    let mut report = create_private(&dir.join("bottlenecks.md"))?;
    writeln!(report,"# HyperDU / LogMosaic 診断\n\nLogMosaicが出力し、保持検証を通過したイベントを分析しています。別走査の結果は混在させません。\n")?;
    writeln!(
        report,
        "走査時間（ns）: `{}`。イベント数: `{}`。\n",
        summary["scan_wall_ns"],
        events.len()
    )?;
    writeln!(report,"## CPU sample 上位\n\n割合はCPU sample内の割合です。wall時間の内訳ではありません。sampleなしはCPU負荷ゼロを意味しません。\n\n| Symbol | Samples |\n|---|---:|")?;
    for row in summary["cpu"]["hotspots"]
        .as_array()
        .context("hotspot array")?
    {
        let label = row["symbol"]
            .as_str()
            .unwrap_or("unknown")
            .replace('|', "\\|")
            .replace(['\r', '\n'], " ");
        writeln!(report, "| {} | {} |", label, row["samples"])?;
    }
    writeln!(report,"\n## Syscall\n\n番号は測定OS/architectureに依存します。時間は同じPID/TIDのentry/exit間隔で、CPU実行と待機を含みます。複数thread分をwall時間に加算しません。\n\n| Number | Entries | Matched | Sum thread ns | Max ns |\n|---|---:|---:|---:|---:|")?;
    for row in summary["syscalls"]["by_number"]
        .as_array()
        .context("syscall array")?
    {
        writeln!(
            report,
            "| {} | {} | {} | {} | {} |",
            row["number"],
            row["entries"],
            row["matched_pairs"],
            row["summed_thread_elapsed_ns"],
            row["max_elapsed_ns"]
        )?;
    }
    writeln!(report,"\n未対応/欠損sourceは `result.json` を確認してください。`bottlenecks.json` にworkerごとのdirectory処理量、process counter差分、allocation累積値を保存しています。`timeline.trace.json` はTrace Event形式で、同時実行されたイベントを時間軸表示できます。原因の断定や改善倍率の推測は行いません。")?;
    Ok(
        json!({"summary":"bottlenecks.json","report":"bottlenecks.md","timeline":"timeline.trace.json", "events":events.len()}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::Clock;
    #[test]
    fn overlapping_threads_do_not_become_wall_time() {
        let c = Clock {
            mono_ns: 100,
            unix_ns: 1000,
            uncertainty_ns: 1,
            domain: "test".into(),
        };
        let run = "e".repeat(32);
        let ev = |seq, time, tid, name: &str| {
            Event::new(
                &c,
                &run,
                "perf",
                seq,
                time,
                2,
                tid,
                "perf.event",
                json!({
            "perf.event":name,"perf.detail":"NR 332 (0)","profile.category":"syscall"}),
            )
            .unwrap()
        };
        let events = vec![
            ev(0, 100, 1, "raw_syscalls:sys_enter"),
            ev(1, 110, 2, "raw_syscalls:sys_enter"),
            ev(2, 200, 1, "raw_syscalls:sys_exit"),
            ev(3, 210, 2, "raw_syscalls:sys_exit"),
        ];
        let summary = summarize(&events);
        assert_eq!(
            summary["syscalls"]["by_number"][0]["summed_thread_elapsed_ns"],
            200
        );
        assert_eq!(summary["syscalls"]["by_number"][0]["matched_pairs"], 2);
        assert_eq!(summary["scan_wall_ns"], Value::Null);
        assert_eq!(summary["cpu"]["status"], "no_samples");
        assert_eq!(
            trace_events(&events)["traceEvents"]
                .as_array()
                .unwrap()
                .len(),
            4
        );
    }
    #[test]
    fn missing_syscall_entry_is_not_a_zero_duration_pair() {
        let c = Clock::capture().unwrap();
        let event = Event::new(
            &c,
            &"e".repeat(32),
            "perf",
            0,
            c.mono_ns,
            2,
            2,
            "perf.event",
            json!({
            "perf.event":"raw_syscalls:sys_exit","perf.detail":"NR 332 = 0"}),
        )
        .unwrap();
        let summary = summarize(&[event]);
        assert_eq!(summary["syscalls"]["unmatched_records"], 1);
        assert!(summary["syscalls"]["by_number"]
            .as_array()
            .unwrap()
            .is_empty());
    }
}
