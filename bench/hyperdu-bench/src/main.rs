use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use anyhow::{ensure, Context, Result};
use clap::{Args, Parser, Subcommand};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use hyperdu_bench::{clock::{mono_ns,Clock}, create_private, dataset::{self,Oracle}, logmosaic,
    median, perf::Perf, process::{self,OwnedChild}, read_json, sha256, write_json, Event};

#[derive(Parser)]
#[command(about="HyperDU core benchmark and concurrent LogMosaic diagnostics", version)]
struct Cli { #[command(subcommand)] action: Action }
#[derive(Subcommand)]
enum Action {
    Dataset { #[arg(long)] root: PathBuf, #[arg(long,default_value="wide")] shape: String,
        #[arg(long,default_value_t=100_000)] files:u64, #[arg(long,default_value_t=4096)] bytes:usize },
    Run { #[command(flatten)] target: Target, #[arg(long)] base_driver: Option<PathBuf>,
        #[arg(long,default_value_t=8)] runs:usize },
    Diagnose { #[command(flatten)] target: Target, #[arg(long)] logmosaic_agent:PathBuf,
        #[arg(long)] perf:Option<PathBuf> },
    #[command(hide=true)] Probe,
}
#[derive(Args)]
struct Target {
    #[arg(long)] root:PathBuf,
    #[arg(long)] driver:PathBuf,
    #[arg(long)] output:PathBuf,
    #[arg(long,default_value_t=0)] threads:usize,
    #[arg(long,default_value_t=300)] timeout_secs:u64,
}

fn output_directory(requested: &Path, root: &Path) -> Result<PathBuf> {
    let parent = requested.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new(".")).canonicalize()?;
    let out = parent.join(requested.file_name().context("output needs a directory name")?);
    ensure!(!out.starts_with(root),"output must be outside the dataset");
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)] { use std::os::unix::fs::DirBuilderExt; builder.mode(0o700); }
    builder.create(&out).context("output must be a new directory")?;
    Ok(out.canonicalize()?)
}

fn check_result(value: &Value, expected: &Oracle) -> Result<()> {
    ensure!(value["schema_version"]==1 && value["surface"]=="hyperdu-core-driver-v1","unsupported driver contract");
    ensure!(value["errors"]==0,"scanner errors invalidate a sample");
    for (key,want) in [("files",expected.files),("directories",expected.directories),("logical",expected.logical)] {
        ensure!(value[key].as_u64()==Some(want),"oracle mismatch: {key}");
    }
    ensure!(value["scan_ns"].as_u64().is_some(),"missing scan duration");
    Ok(())
}

fn driver_command(driver:&Path,root:&Path,dir:&Path,threads:usize) -> Command {
    let mut c = Command::new(driver);
    c.arg("--root").arg(root).arg("--result").arg(dir.join("driver.json"))
        .arg("--threads").arg(threads.to_string()); c
}

fn quiet(command:&mut Command,dir:&Path) -> Result<()> {
    command.stdin(Stdio::null()).stdout(create_private(&dir.join("target.stdout"))?)
        .stderr(create_private(&dir.join("target.stderr"))?);
    Ok(())
}

fn sample(driver:&Path,root:&Path,dir:&Path,threads:usize,timeout:Duration,expected:&Oracle) -> Result<Value> {
    fs::create_dir(dir)?;
    let mut cmd = driver_command(driver,root,dir,threads);
    quiet(&mut cmd,dir)?;
    let start = Instant::now();
    let status = OwnedChild::spawn(&mut cmd)?.wait(timeout)?;
    let wall_ns:u64 = start.elapsed().as_nanos().try_into()?;
    ensure!(status.success(),"driver failed: {status}");
    let mut v = read_json(&dir.join("driver.json"))?;
    check_result(&v,expected)?;
    v["observed_process_elapsed_ns"] = json!(wall_ns);
    v["completion_poll_interval_ns"] = json!(2_000_000);
    Ok(v)
}

fn benchmark(t:&Target,root:&Path,driver:&Path,out:&Path,base:Option<PathBuf>,runs:usize,expected:&Oracle) -> Result<Value> {
    ensure!(runs>=4 && runs<=1000 && runs%2==0,"runs must be even and in 4..=1000");
    let mut drivers = vec![driver.to_path_buf()];
    if let Some(b) = base { let b = b.canonicalize()?; ensure!(!b.starts_with(root),"driver inside dataset"); drivers.push(b); }
    let hashes = drivers.iter().map(|p| sha256(p)).collect::<Result<Vec<_>>>()?;
    let timeout = Duration::from_secs(t.timeout_secs);
    for (i,d) in drivers.iter().enumerate() { sample(d,root,&out.join(format!("warmup-{i}")),t.threads,timeout,expected)?; }
    let mut journal = create_private(&out.join("samples.jsonl"))?;
    let mut samples = Vec::new(); let mut schedule = Vec::new();
    for round in 0..runs {
        let order:Vec<_> = if round%2==0 {(0..drivers.len()).collect()} else {(0..drivers.len()).rev().collect()};
        schedule.push(order.clone());
        for i in order {
            let v = sample(&drivers[i],root,&out.join(format!("round-{round}-tool-{i}")),t.threads,timeout,expected)?;
            let record = json!({"round":round,"tool":i,"measurement":v});
            serde_json::to_writer(&mut journal,&record)?; journal.write_all(b"\n")?; journal.flush()?;
            samples.push(record);
        }
    }
    ensure!(dataset::oracle(root)?==*expected,"dataset changed");
    for (d,h) in drivers.iter().zip(&hashes) { ensure!(sha256(d)?==*h,"driver changed"); }
    let medians:Vec<_> = (0..drivers.len()).map(|i| median(&samples.iter().filter(|s|s["tool"]==i)
        .map(|s|s["measurement"]["scan_ns"].as_u64().unwrap()).collect::<Vec<_>>()).unwrap()).collect();
    let ratio = if medians.len()==2 && medians[0]>0 {Some(medians[1] as f64 / medians[0] as f64)} else {None};
    Ok(json!({"schema_version":1,"complete":true,"mode":"benchmark","surface":"hyperdu-core-driver-v1",
        "cache":"warm metadata; oracle and warmup precede timed samples","oracle":expected,
        "runner_sha256":sha256(&std::env::current_exe()?)?,"driver_sha256":hashes,"schedule":schedule,
        "samples":samples,"median_scan_ns":medians,"base_over_candidate":ratio,
        "environment":{"os":std::env::consts::OS,"arch":std::env::consts::ARCH,
            "available_parallelism":std::thread::available_parallelism().map(|x|x.get()).ok()},
        "statistics":"median; no outlier deletion; descriptive ratio, not significance test"}))
}

fn diagnose(t:&Target,root:&Path,driver:&Path,out:&Path,agent:&Path,perf_binary:Option<PathBuf>,expected:&Oracle) -> Result<Value> {
    let agent = agent.canonicalize()?;
    ensure!(!agent.starts_with(root),"collector binary inside dataset");
    let digest = sha256(driver)?;
    let clock = Clock::capture()?;
    write_json(&out.join("clock.json"),&clock)?;
    let run = format!("{:x}",Sha256::digest(format!("{}:{}:{}",clock.unix_ns,std::process::id(),out.display()).as_bytes()))[..32].to_owned();
    let profiler = if cfg!(target_os="linux") {
        perf_binary.map(|p| Perf::probe(&p,out)).transpose()?
    } else { None };
    let using_perf = profiler.as_ref().is_some_and(|p|!p.events.is_empty());
    let mut cmd = if using_perf {profiler.as_ref().unwrap().command(driver,out)} else {Command::new(driver)};
    cmd.arg("--root").arg(root).arg("--result").arg(out.join("driver.json"))
        .arg("--threads").arg(t.threads.to_string())
        .arg("--events").arg(out.join("internal.jsonl"))
        .arg("--clock").arg(out.join("clock.json"))
        .arg("--run-id").arg(&run)
        .arg("--ready-file").arg(out.join("ready.json"))
        .arg("--go-file").arg(out.join("go"));
    quiet(&mut cmd,out)?;
    let started = Instant::now();
    let mut child = OwnedChild::spawn(&mut cmd)?;
    let target_pid = loop {
        if let Ok(v) = read_json(&out.join("ready.json")) {
            if let Some(pid) = v["pid"].as_u64().and_then(|p|u32::try_from(p).ok()) { break pid; }
        }
        ensure!(child.0.try_wait()?.is_none(),"driver/perf exited before readiness; see target.stderr");
        ensure!(started.elapsed()<Duration::from_secs(10),"driver readiness timed out");
        std::thread::sleep(Duration::from_millis(1));
    };
    let mut events = Vec::new(); let mut metric_failures = 0u64; let mut metric_samples = 0u64;
    create_private(&out.join("go"))?;
    let released = mono_ns()?;
    loop {
        if let Some(status) = child.0.try_wait()? { ensure!(status.success(),"driver/perf failed: {status}"); break; }
        ensure!(started.elapsed()<Duration::from_secs(t.timeout_secs),"diagnostic target timed out");
        let now = mono_ns()?;
        match process::snapshot(target_pid) {
            Ok(mut attrs) => {
                attrs["profile.category"] = json!("process");
                events.push(Event::new(&clock,&run,"process",metric_samples,now,target_pid,0,"process.sample",attrs)?);
                metric_samples += 1;
            }
            Err(_) => { metric_failures += 1; }
        }
        ensure!(events.len()<hyperdu_bench::MAX_EVENTS,"process event limit exceeded");
        std::thread::sleep(Duration::from_millis(5));
    }
    let finished = mono_ns()?;
    let driver_result = read_json(&out.join("driver.json"))?;
    check_result(&driver_result,expected)?;
    ensure!(driver_result["pid"]==target_pid,"driver identity changed");
    events.extend(logmosaic::read_events(&out.join("internal.jsonl"))?);
    let perf_status = if using_perf {
        let (p,status) = profiler.as_ref().unwrap().decode(out,&clock,&run,target_pid)?;
        events.extend(p); status
    } else {json!({"status":"unavailable","reason":"no supported perf events, no --perf, or unsupported OS",
        "capabilities":profiler.as_ref().map(|p|&p.capability)})};
    events.push(Event::new(&clock,&run,"runner",0,released,target_pid,0,"benchmark.interval",
        json!({"profile.category":"benchmark","duration_ns":finished-released,
            "measurement.kind":"start gate to process observation; includes diagnostics and shutdown"}))?);
    let internal_count = events.iter().filter(|e|e.attributes["profile.category"]=="internal").count();
    let allocation_count = events.iter().filter(|e|e.attributes["profile.category"]=="allocation").count();
    ensure!(internal_count>0 && allocation_count>0,"driver did not emit required internal/allocation events");
    ensure!(dataset::oracle(root)?==*expected,"dataset changed during diagnosis");
    ensure!(sha256(driver)?==digest,"driver changed during diagnosis");
    logmosaic::timeline(&out.join("timeline.jsonl"),&run,events)?;
    let delivery = logmosaic::ingest(&agent,out,Duration::from_secs(t.timeout_secs))?;
    Ok(json!({"schema_version":1,"complete":true,"mode":"diagnose","surface":"hyperdu-core-driver-v1",
        "experiment_id":run,"clock":clock,"concurrent_collection":true,"workload_executions":1,
        "cache":"warm metadata; independent oracle precedes the one diagnosed scan",
        "oracle":expected,"driver_sha256":digest,"driver_result":driver_result,
        "runner_sha256":sha256(&std::env::current_exe()?)?,"logmosaic":delivery,
        "sources":{"process":{"status":if metric_samples>0 {"collected"} else {"no_samples"},
            "samples":metric_samples,"read_failures":metric_failures,"interval_ms":5},
            "internal":{"events":internal_count,"dropped":driver_result["telemetry_dropped"]},
            "allocation":{"events":allocation_count,"kind":"cumulative requested-allocation counters, not stacks"},
            "perf":perf_status,
            "disk_latency":{"status":"unsupported","reason":"process I/O counters do not establish physical I/O latency"},
            "full_scheduler":{"status":"unsupported","reason":"per-task context-switch records are not complete ready/block intervals"}},
        "profile_complete":false,
        "profile_completeness_note":"See per-source status; physical disk latency and full scheduler attribution are not implemented"}))
}

fn execute(t:Target,base:Option<PathBuf>,runs:usize,diagnostic:Option<(PathBuf,Option<PathBuf>)>) -> Result<()> {
    ensure!(t.timeout_secs>0 && t.timeout_secs<=3600 && t.threads<=4096,"invalid safety limits");
    let root = t.root.canonicalize()?; let driver = t.driver.canonicalize()?;
    ensure!(!driver.starts_with(&root),"driver must be outside dataset");
    let out = output_directory(&t.output,&root)?;
    write_json(&out.join("state.json"),&json!({"schema_version":1,"complete":false,"stage":"started"}))?;
    let outcome = (|| -> Result<Value> {
        let expected = dataset::oracle(&root)?;
        if let Some((agent,perf)) = diagnostic {diagnose(&t,&root,&driver,&out,&agent,perf,&expected)}
        else {benchmark(&t,&root,&driver,&out,base,runs,&expected)}
    })();
    match outcome {
        Ok(value) => {
            write_json(&out.join("result.json"),&value)?;
            let mut report = create_private(&out.join("report.md"))?;
            writeln!(report,"# HyperDU benchmark evidence\n\nSurface: `hyperdu-core-driver-v1` (not the released CLI).\n\n```json\n{}\n```",serde_json::to_string_pretty(&value)?)?;
            println!("{}",out.join("result.json").display()); Ok(())
        }
        Err(e) => {write_json(&out.join("failure.json"),&json!({"complete":false,"error":format!("{e:#}")}))?; Err(e)}
    }
}
fn main() -> Result<()> {
    match Cli::parse().action {
        Action::Probe => Ok(()),
        Action::Dataset{root,shape,files,bytes} => {println!("{}",serde_json::to_string_pretty(&dataset::generate(&root,&shape,files,bytes)?)?); Ok(())},
        Action::Run{target,base_driver,runs} => execute(target,base_driver,runs,None),
        Action::Diagnose{target,logmosaic_agent,perf} => execute(target,None,0,Some((logmosaic_agent,perf))),
    }
}
