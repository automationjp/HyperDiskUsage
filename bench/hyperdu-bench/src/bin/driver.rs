//! A separately identified core-engine surface, not the released HyperDU CLI.
use anyhow::{ensure, Result};
use clap::Parser;
use hyperdu_bench::{
    clock::{mono_ns, Clock},
    create_private, write_json, Event, MAX_EVENTS, MAX_JSON_BYTES,
};
use hyperdu_core::{DirContext, FileSystemScanner, Options, PlatformScanner, ScanContext, StatMap};
use serde_json::{json, Value};
use std::alloc::{GlobalAlloc, Layout};
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

static COUNT: AtomicBool = AtomicBool::new(false);
static ALLOCS: AtomicU64 = AtomicU64::new(0);
static ALLOC_BYTES: AtomicU64 = AtomicU64::new(0);
static FREES: AtomicU64 = AtomicU64::new(0);
static FREE_BYTES: AtomicU64 = AtomicU64::new(0);
static REALLOCS: AtomicU64 = AtomicU64::new(0);

struct CountingAllocator;
#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

// Only atomics in allocator callbacks: no formatting, logging, allocation or locks.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = mimalloc::MiMalloc.alloc(layout);
        if !p.is_null() && COUNT.load(Ordering::Relaxed) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
            ALLOC_BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        }
        p
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let p = mimalloc::MiMalloc.alloc_zeroed(layout);
        if !p.is_null() && COUNT.load(Ordering::Relaxed) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
            ALLOC_BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, layout: Layout) {
        if COUNT.load(Ordering::Relaxed) {
            FREES.fetch_add(1, Ordering::Relaxed);
            FREE_BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        }
        mimalloc::MiMalloc.dealloc(p, layout);
    }
    unsafe fn realloc(&self, p: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let new = mimalloc::MiMalloc.realloc(p, layout, new_size);
        if !new.is_null() && COUNT.load(Ordering::Relaxed) {
            REALLOCS.fetch_add(1, Ordering::Relaxed);
            ALLOC_BYTES.fetch_add(new_size as u64, Ordering::Relaxed);
            FREE_BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        }
        new
    }
}

fn allocation() -> Value {
    json!({"allocation.alloc_calls":ALLOCS.load(Ordering::Relaxed),
        "allocation.free_calls":FREES.load(Ordering::Relaxed),
        "allocation.realloc_calls":REALLOCS.load(Ordering::Relaxed),
        "allocation.requested_bytes":ALLOC_BYTES.load(Ordering::Relaxed),
        "allocation.freed_requested_bytes":FREE_BYTES.load(Ordering::Relaxed),
        "profile.category":"allocation",
        "measurement.scope":"driver process during scan, including instrumentation; requested sizes, not RSS",
        "measurement.kind":"cumulative counters; no allocation stacks"})
}

fn tid() -> u64 {
    #[cfg(target_os = "linux")]
    {
        unsafe { libc::syscall(libc::SYS_gettid) as u64 }
    }
    #[cfg(windows)]
    {
        unsafe { windows_sys::Win32::System::Threading::GetCurrentThreadId() as u64 }
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        0
    }
}

struct Sink {
    clock: Clock,
    run: String,
    writer: Mutex<BufWriter<File>>,
    seq: AtomicU64,
    bytes: AtomicU64,
    dropped: AtomicU64,
}
impl Sink {
    fn emit(&self, name: &str, start: u64, attrs: Value) {
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        let result = (|| -> Result<()> {
            ensure!(seq < MAX_EVENTS as u64, "event limit");
            let event = Event::new(
                &self.clock,
                &self.run,
                "hyperdu",
                seq,
                start,
                std::process::id(),
                tid(),
                name,
                attrs,
            )?;
            let line = serde_json::to_vec(&event)?;
            let bytes = self
                .bytes
                .fetch_add(line.len() as u64 + 1, Ordering::Relaxed);
            ensure!(bytes + (line.len() as u64) < MAX_JSON_BYTES, "byte limit");
            let mut w = self
                .writer
                .lock()
                .map_err(|_| anyhow::anyhow!("event writer poisoned"))?;
            w.write_all(&line)?;
            w.write_all(b"\n")?;
            w.flush()?;
            Ok(())
        })();
        if result.is_err() {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

struct Scanner {
    sink: Option<Arc<Sink>>,
}
impl FileSystemScanner for Scanner {
    fn process_dir(&self, ctx: &ScanContext, dctx: &DirContext, map: &mut StatMap) {
        if let Some(sink) = &self.sink {
            let start = mono_ns().ok();
            PlatformScanner.process_dir(ctx, dctx, map);
            if let (Some(start), Ok(end)) = (start, mono_ns()) {
                sink.emit("hyperdu.directory",start,json!({"profile.category":"internal",
                    "duration_ns":end.saturating_sub(start), "measurement.kind":"wall interval on worker; nested/parallel intervals are not additive"}));
            } else {
                sink.dropped.fetch_add(1, Ordering::Relaxed);
            }
        } else {
            PlatformScanner.process_dir(ctx, dctx, map);
        }
    }
}

#[derive(Parser)]
struct Args {
    #[arg(long)]
    root: PathBuf,
    #[arg(long)]
    result: PathBuf,
    #[arg(long, default_value_t = 0)]
    threads: usize,
    #[arg(long)]
    events: Option<PathBuf>,
    #[arg(long)]
    clock: Option<PathBuf>,
    #[arg(long)]
    run_id: Option<String>,
    #[arg(long)]
    ready_file: Option<PathBuf>,
    #[arg(long)]
    go_file: Option<PathBuf>,
}

fn main() -> Result<()> {
    let args = Args::parse();
    ensure!(args.threads <= 4096, "thread budget exceeded");
    let root = args.root.canonicalize()?;
    let sink = if let Some(path) = &args.events {
        let c: Clock = serde_json::from_reader(File::open(
            args.clock
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("--clock required"))?,
        )?)?;
        let run = args
            .run_id
            .clone()
            .ok_or_else(|| anyhow::anyhow!("--run-id required"))?;
        Some(Arc::new(Sink {
            clock: c,
            run,
            writer: Mutex::new(BufWriter::new(create_private(path)?)),
            seq: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
        }))
    } else {
        None
    };
    ensure!(
        args.ready_file.is_some() == args.go_file.is_some(),
        "ready/go must be supplied together"
    );
    if let (Some(ready), Some(go)) = (&args.ready_file, &args.go_file) {
        write_json(ready, &json!({"pid":std::process::id()}))?;
        let wait = Instant::now();
        while !go.exists() {
            ensure!(
                wait.elapsed() < Duration::from_secs(15),
                "start gate timed out"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    let mut opt = Options::default();
    if args.threads > 0 {
        opt.threads = args.threads;
    }
    opt.compute_physical = false;
    opt.one_file_system = true;
    opt.follow_links = false;
    opt.progress_every = 0;
    let stop = Arc::new(AtomicBool::new(false));
    let reporter = sink.as_ref().map(|s| {
        let s = s.clone();
        let stop = stop.clone();
        std::thread::spawn(move || {
            while !stop.load(Ordering::Acquire) {
                if let Ok(now) = mono_ns() {
                    s.emit("hyperdu.allocation", now, allocation());
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        })
    });
    if let Some(s) = &sink {
        if let Ok(mut attrs) = hyperdu_bench::process::snapshot(std::process::id()) {
            attrs["profile.category"] = json!("process");
            s.emit("process.scan_start", mono_ns()?, attrs);
        }
    }
    COUNT.store(sink.is_some(), Ordering::Relaxed);
    let begin = mono_ns()?;
    let timer = Instant::now();
    let scanned =
        hyperdu_core::scan_directory_with(&root, &opt, Arc::new(Scanner { sink: sink.clone() }));
    let scan_ns: u64 = timer.elapsed().as_nanos().try_into()?;
    COUNT.store(false, Ordering::Relaxed);
    if let Some(s) = &sink {
        if let Ok(mut attrs) = hyperdu_bench::process::snapshot(std::process::id()) {
            attrs["profile.category"] = json!("process");
            s.emit("process.scan_end", mono_ns()?, attrs);
        }
    }
    stop.store(true, Ordering::Release);
    if let Some(reporter) = reporter {
        reporter
            .join()
            .map_err(|_| anyhow::anyhow!("allocation reporter panicked"))?;
    }
    if let Some(s) = &sink {
        s.emit(
            "hyperdu.scan",
            begin,
            json!({"profile.category":"internal","duration_ns":scan_ns,
            "measurement.kind":"core scan including aggregation; not released CLI startup/output"}),
        );
        s.emit("hyperdu.allocation", mono_ns()?, allocation());
    }
    let map = scanned?;
    let total = map
        .get(&root)
        .ok_or_else(|| anyhow::anyhow!("root result missing"))?;
    let errors = opt.error_count.load(Ordering::Relaxed);
    let dropped = sink
        .as_ref()
        .map_or(0, |s| s.dropped.load(Ordering::Relaxed));
    write_json(
        &args.result,
        &json!({"schema_version":1,"surface":"hyperdu-core-driver-v1",
        "pid":std::process::id(),"threads":opt.threads,"logical":total.logical,"files":total.files,
        "directories":map.len(),"errors":errors,"scan_ns":scan_ns,
        "telemetry_dropped":dropped,"allocation":if sink.is_some() {allocation()} else {json!({"status":"disabled"})}}),
    )?;
    ensure!(errors == 0, "scanner returned {errors} errors");
    let _ = fs::metadata(&args.result)?;
    Ok(())
}
