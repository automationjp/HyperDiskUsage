#[cfg(feature = "mimalloc")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        Arc,
    },
};

use ahash::AHashMap as HashMap;
use aho_corasick::AhoCorasick;
use anyhow::{anyhow, Result};
use crossbeam_deque::Worker;
use crossbeam_utils::Backoff;
use dashmap::DashMap;
use globset::{Glob, GlobSet, GlobSetBuilder};
use regex::RegexSet;
use serde::Serialize;

pub mod classify;
mod common_ops;
mod error_handling;
mod filters; // centralize filter helpers
pub mod fs_strategy;
/// Directory-level aggregates for the persistent index (#16). Not yet wired to
/// a scan or to storage; see `docs/design/persistent-index.md`.
pub mod index;
pub mod memory_pool;
mod options; // for OptionsBuilder
mod platform;
mod rollup;
mod scanner; // FileSystemScanner + platform default
mod scheduler;
mod tuning;

pub use options::{
    CompatConfig, FilterConfig, OptionsBuilder, OutputConfig, PerformanceConfig, TuningConfig,
    WindowsConfig,
};
#[cfg(feature = "rayon-par")]
pub use scanner::auto_parallel_scan;
#[cfg(feature = "rayon-par")]
pub use scanner::parallel_scan;
pub use scanner::{platform_scanner, FileSystemScanner, PlatformScanner};
pub(crate) use scheduler::{Job, Scheduler};

pub(crate) use crate::filters::path_excluded;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompatMode {
    HyperDU,
    GnuBasic,
    GnuStrict,
    PosixStrict,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeuristicsMode {
    Auto,
    OuterOnly,
    InnerOnly,
}

/// Called with the running file count as a scan progresses.
pub type ProgressCallback = Arc<dyn Fn(u64) + Send + Sync + 'static>;

/// A file the scan happened to be looking at when a progress tick fired.
///
/// The sizes come from metadata the scan already read. Callers must not stat
/// the path again: doing so doubled the metadata I/O of every progress tick,
/// on a path chosen essentially at random.
pub struct ProgressSample<'a> {
    pub path: &'a Path,
    /// Size the filesystem reports for the file.
    pub logical: u64,
    /// Space actually allocated, or the logical size when physical sizes are
    /// not being computed.
    pub physical: u64,
}

/// Called occasionally with a sample file, to show what a scan is working on.
pub type ProgressSampleCallback = Arc<dyn Fn(&ProgressSample<'_>) + Send + Sync + 'static>;

/// How hard a scan may lean on the storage.
///
/// Orthogonal to [`CompatMode`], which decides what the numbers mean, and to
/// the CLI's `--perf`, which trades accuracy for speed. This decides how much
/// I/O the scan is willing to cause on the way to the same answer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum IoProfile {
    /// Take everything the hardware will give.
    Throughput,
    /// Correct answers without deliberately warming the page cache.
    #[default]
    Balanced,
    /// Stay out of the way of whatever else is using the disk: no readahead,
    /// few workers, and no splitting of large directories.
    Gentle,
}

/// Called with a formatted message for every error a scan runs into.
pub type ErrorReporter = Arc<dyn Fn(&str) + Send + Sync + 'static>;

#[derive(Default, Clone, Copy, Serialize, Debug)]
pub struct Stat {
    pub logical: u64,
    pub physical: u64,
    pub files: u64,
}

impl Stat {
    #[inline]
    pub(crate) fn add(&mut self, other: &Stat) {
        self.logical += other.logical;
        self.physical += other.physical;
        self.files += other.files;
    }
}

#[derive(Clone)]
pub struct Options {
    pub exclude_contains: Vec<String>,
    pub max_depth: u32,     // 0 = unlimited
    pub min_file_size: u64, // bytes
    pub follow_links: bool,
    pub threads: usize,
    pub progress_every: u64, // 0 = disabled
    pub progress_callback: Option<ProgressCallback>,
    pub progress_sample_callback: Option<ProgressSampleCallback>,
    pub compute_physical: bool, // if false, use logical size as physical (faster)
    pub dir_yield_every: Arc<AtomicUsize>, // 0 = no yielding; split large dirs every N entries
    pub approximate_sizes: bool, // if true and compute_physical=false, estimate regular file size (e.g., 4KiB) to avoid statx
    pub active_threads: Arc<AtomicUsize>, // runtime-tunable active worker threads (<= threads)
    pub cancel: Arc<AtomicBool>, // cooperative cancellation
    pub exclude_ac: Option<AhoCorasick>,
    pub exclude_regex: Vec<String>,
    pub exclude_glob: Vec<String>,
    pub exclude_regex_set: Option<RegexSet>,
    pub exclude_glob_set: Option<GlobSet>,
    /// Compiled by the scan bootstrap: `exclude_contains` as UTF-16 for name-level
    /// matching on Windows without per-entry string conversion.
    #[doc(hidden)]
    pub exclude_contains_w: Vec<Vec<u16>>,
    /// Compiled by the scan bootstrap: true when any filter needs the full path
    /// (glob/regex present, or a contains-pattern includes a path separator).
    /// When false, backends may skip building child paths for files entirely.
    #[doc(hidden)]
    pub needs_path_filter: bool,
    /// Filled in by the scan bootstrap: identifier of the filesystem holding the
    /// scan root, as the platform reports it (packed `dev_t` on Linux, volume
    /// serial on Windows). `--one-file-system` compares against this rather than
    /// against each parent, which is what GNU du means by "the starting point".
    #[doc(hidden)]
    pub root_fs_id: u64,
    /// How much I/O the scan may cause. See [`IoProfile`].
    pub io_profile: IoProfile,
    /// Ask the kernel to read ahead of the scan. `None` lets the profile decide.
    /// Readahead shortens latency but raises total bytes read, so it is off
    /// unless asked for.
    pub prefetch: Option<bool>,
    /// Resolved by the scan bootstrap from `prefetch` and `io_profile`.
    #[doc(hidden)]
    pub io_prefetch: bool,
    /// getdents64 buffer size in bytes (Linux). Set per scan by
    /// [`fs_strategy`], because the best size depends on the filesystem. Held
    /// here rather than in an environment variable so two scans of different
    /// filesystems in one process do not fight over a single global.
    pub getdents_buf_bytes: usize,
    // Compatibility and correctness knobs
    pub compat_mode: CompatMode,
    pub count_hardlinks: bool, // if true, count hardlinks as separate (non-GNU). Default false = dedupe hardlinks like GNU du
    pub inode_cache: Option<Arc<DashMap<(u64, u64), ()>>>, // (dev, ino)
    pub error_count: Arc<AtomicU64>,
    pub error_report: Option<ErrorReporter>,
    pub one_file_system: bool,
    pub visited_bloom: Option<Arc<Bloom>>, // fast pre-check
    pub visited_dirs: Option<Arc<DashMap<(u64, u64), ()>>>, // loop detection when following links
    // Keep progress lightweight: we intentionally do not accumulate sizes per-file here.
    // Adaptive tuning / scheduling preferences (configured by CLI config)
    pub tune_enabled: bool,
    pub tune_interval_ms: u64,
    pub heuristics_mode: HeuristicsMode,
    pub prefer_inner_rayon: bool,
    /// Legacy Windows knob, kept for configuration compatibility. The Windows
    /// backend now obtains file ids and allocation sizes from directory
    /// enumeration and never opens per-file handles, so this has no effect.
    pub win_allow_handle: bool,
    /// Legacy Windows knob (see `win_allow_handle`). No effect.
    pub win_handle_sample_every: u64,
}

impl std::fmt::Debug for Options {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Options")
            .field("exclude_contains", &self.exclude_contains)
            .field("max_depth", &self.max_depth)
            .field("min_file_size", &self.min_file_size)
            .field("follow_links", &self.follow_links)
            .field("threads", &self.threads)
            .field("progress_every", &self.progress_every)
            .finish()
    }
}

impl Default for Options {
    fn default() -> Self {
        let threads_default = default_threads();
        Self {
            // Nothing is excluded by default. A disk-usage tool that silently
            // drops directories misreports where the space went, and the old
            // defaults matched by substring: ".git" also swallowed ".github".
            // Pass --exclude .git,node_modules,target for the previous behaviour.
            exclude_contains: Vec::new(),
            max_depth: 0,
            min_file_size: 0,
            follow_links: false,
            threads: threads_default,
            progress_every: 0,
            progress_callback: None,
            progress_sample_callback: None,
            io_profile: IoProfile::default(),
            prefetch: None,
            io_prefetch: false,
            getdents_buf_bytes: default_getdents_buf_bytes(),
            compute_physical: true,
            dir_yield_every: Arc::new(AtomicUsize::new(
                std::env::var("HYPERDU_DIR_YIELD_EVERY")
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0),
            )),
            approximate_sizes: false,
            active_threads: Arc::new(AtomicUsize::new(threads_default.max(1))),
            exclude_ac: None,
            exclude_regex: Vec::new(),
            exclude_glob: Vec::new(),
            exclude_regex_set: None,
            exclude_glob_set: None,
            exclude_contains_w: Vec::new(),
            needs_path_filter: false,
            root_fs_id: 0,
            compat_mode: CompatMode::HyperDU,
            count_hardlinks: false,
            inode_cache: None,
            error_count: Arc::new(AtomicU64::new(0)),
            error_report: None,
            one_file_system: false,
            visited_bloom: None,
            visited_dirs: None,
            cancel: Arc::new(AtomicBool::new(false)),
            tune_enabled: false,
            tune_interval_ms: 800,
            heuristics_mode: HeuristicsMode::Auto,
            prefer_inner_rayon: false,
            win_allow_handle: false,
            win_handle_sample_every: 64,
        }
    }
}

// Lightweight Bloom filter for (dev,ino) pairs to reduce HashMap lookups
pub struct Bloom {
    mask: usize,
    bits: Box<[AtomicU64]>,
}
impl Bloom {
    pub fn with_bits(n_bits: usize) -> Self {
        let n = n_bits.next_power_of_two().max(1 << 20); // at least ~1M bits
        let words = n.div_ceil(64);
        let mut v: Vec<AtomicU64> = Vec::with_capacity(words);
        v.resize_with(words, || AtomicU64::new(0));
        Self {
            mask: n - 1,
            bits: v.into_boxed_slice(),
        }
    }
    #[inline(always)]
    fn h(x: u128) -> (usize, u64) {
        // simple mix
        let mut v = x ^ (x >> 33);
        v = v.wrapping_mul(0xff51afd7ed558ccd);
        let idx = (v >> 6) as usize;
        let bit = (1u64) << (v as u32 & 63);
        (idx, bit)
    }
    #[inline(always)]
    pub fn test_and_set(&self, dev: u64, ino: u64) -> bool {
        let key = ((dev as u128) << 64) | (ino as u128);
        let (i1, b1) = Self::h(key);
        let (i2, b2) = Self::h(key.rotate_left(17));
        let i1 = i1 & self.mask;
        let i2 = i2 & self.mask;
        let w1 = &self.bits[i1 / 64];
        let w2 = &self.bits[i2 / 64];
        let old1 = w1.fetch_or(b1, Ordering::Relaxed);
        let old2 = w2.fetch_or(b2, Ordering::Relaxed);
        (old1 & b1 != 0) & (old2 & b2 != 0)
    }
}

pub type StatMap = HashMap<PathBuf, Stat>;

/// Default number of worker threads.
///
/// Walking a tree is dominated by blocking metadata syscalls rather than by
/// computation, and a thread waiting inside `statx` holds no CPU. Running more
/// workers than cores therefore raises the number of requests the storage sees
/// at once, which is exactly what a cold scan is limited by.
///
/// Measured on 2 vCPU / gp3 against a Linux kernel checkout (95,953 files),
/// best of two cold runs and five warm runs:
///
/// | threads      | cold    | warm  |
/// |--------------|---------|-------|
/// | 1            | 4483 ms | 273 ms|
/// | 2 (one/core) | 2321 ms | 212 ms|
/// | 8 (this)     |  855 ms | 217 ms|
/// | 32           |  862 ms | 236 ms|
///
/// A warm scan gives up a few percent to the extra threads, and a very small
/// tree pays the spawn cost of roughly 0.3 ms per thread, so the multiplier is
/// capped. Set `Options::threads` when a specific count is wanted.
/// Cycle detection is active: links are followed and a visited set exists.
#[inline]
pub(crate) fn follows_links(opt: &Options) -> bool {
    opt.follow_links && opt.visited_dirs.is_some()
}

/// Worker count for this scan: the requested number, capped by the I/O profile.
///
/// [`IoProfile::Gentle`] keeps at most two metadata requests in flight so the
/// scan does not monopolise a queue that other processes are sharing.
pub fn effective_threads(opt: &Options) -> usize {
    let requested = opt.threads.max(1);
    match opt.io_profile {
        IoProfile::Gentle => requested.min(2),
        IoProfile::Balanced | IoProfile::Throughput => requested,
    }
}

/// Process-wide default getdents64 buffer size, overridable with
/// `HYPERDU_GETDENTS_BUF_KB`. A filesystem strategy may override it per scan.
pub fn default_getdents_buf_bytes() -> usize {
    std::env::var("HYPERDU_GETDENTS_BUF_KB")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .map(|kb| kb.max(4) * 1024)
        .unwrap_or(128 * 1024) // NVMe/SSD friendly default
}

pub fn default_threads() -> usize {
    let cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    (cpus * 4).clamp(4, 32)
}

/// Per-worker view of the scan. Constructed on the worker thread and handed to
/// backends by reference; expected to be fully inlined.
#[derive(Clone, Copy)]
pub struct ScanContext<'a> {
    pub(crate) options: &'a Options,
    pub(crate) sched: &'a Scheduler,
    pub(crate) local: &'a Worker<Job>,
    pub(crate) total_files: &'a AtomicU64,
}

#[derive(Clone, Copy)]
pub struct DirContext<'a> {
    pub dir: &'a Path,
    pub depth: u32,
    pub resume: Option<u64>,
}

impl<'a> ScanContext<'a> {
    /// Schedule a discovered subdirectory. Goes to the calling worker's own
    /// deque; idle workers steal from it.
    #[inline]
    pub fn enqueue_dir(&self, path: PathBuf, depth: u32) {
        self.sched.push_local(
            self.local,
            Job {
                dir: path,
                depth,
                resume: None,
            },
        );
    }

    /// Schedule the continuation of a large directory (high priority).
    #[inline]
    pub fn enqueue_resume(&self, path: PathBuf, depth: u32, resume: u64) {
        self.sched.push_high(Job {
            dir: path,
            depth,
            resume: Some(resume),
        });
    }

    /// Progress for one file. The sample carries the sizes the caller already
    /// read, so reporting never costs another `stat`.
    #[inline]
    pub fn report_progress(&self, opt: &Options, sample: Option<(&Path, u64, u64)>) {
        crate::common_ops::report_file_progress(opt, self.total_files, sample);
    }

    /// Batched progress: account for `n` files at once. `sample` is only
    /// evaluated when a callback actually fires.
    #[inline]
    pub fn report_progress_batch(
        &self,
        opt: &Options,
        n: u64,
        sample: impl FnOnce() -> (PathBuf, u64, u64),
    ) {
        crate::common_ops::report_files_batch(opt, self.total_files, n, sample);
    }
}

#[inline]
fn compile_filters_in_place(opt: &mut Options) {
    // Empty patterns must be dropped: an empty needle matches at every position,
    // which would exclude the whole tree.
    let pats: Vec<&str> = opt
        .exclude_contains
        .iter()
        .filter(|s| !s.is_empty())
        .map(|s| s.as_str())
        .collect();
    opt.exclude_ac = if pats.is_empty() {
        None
    } else {
        AhoCorasick::new(&pats).ok()
    };
    if !opt.exclude_regex.is_empty() {
        if let Ok(rs) = RegexSet::new(&opt.exclude_regex) {
            opt.exclude_regex_set = Some(rs);
        }
    } else {
        opt.exclude_regex_set = None;
    }
    if !opt.exclude_glob.is_empty() {
        let mut b = GlobSetBuilder::new();
        for g in &opt.exclude_glob {
            if let Ok(gl) = Glob::new(g) {
                let _ = b.add(gl);
            }
        }
        if let Ok(gs) = b.build() {
            opt.exclude_glob_set = Some(gs);
        }
    } else {
        opt.exclude_glob_set = None;
    }
    opt.exclude_contains_w = opt
        .exclude_contains
        .iter()
        .filter(|s| !s.is_empty())
        .map(|s| s.encode_utf16().collect())
        .collect();
    let contains_has_separator = opt
        .exclude_contains
        .iter()
        .any(|s| s.bytes().any(|c| c == b'/' || c == b'\\'));
    opt.needs_path_filter =
        contains_has_separator || opt.exclude_glob_set.is_some() || opt.exclude_regex_set.is_some();
}

/// Scan several roots, overlapping the ones that sit on different devices.
///
/// Roots on the same device are scanned one after another: they share a queue
/// of physical reads, so overlapping them only adds seeking. Roots on different
/// devices have nothing in common and are scanned at the same time, which turns
/// the total for e.g. `C:\` plus `F:\` from a sum into a maximum.
///
/// Returns one entry per root in the order given, so a caller printing a report
/// per root does not have to re-order anything.
pub fn scan_roots(roots: &[PathBuf], opt: &Options) -> Vec<(PathBuf, Result<StatMap>)> {
    if roots.len() < 2 {
        return roots
            .iter()
            .map(|r| (r.clone(), scan_directory(r, opt)))
            .collect();
    }

    // Group by device, preserving the caller's order within each group.
    let mut groups: Vec<(u64, Vec<usize>)> = Vec::new();
    for (i, r) in roots.iter().enumerate() {
        let dev = platform::filesystem_id(r);
        match groups.iter_mut().find(|(d, _)| *d == dev && dev != 0) {
            Some((_, idx)) => idx.push(i),
            // Device 0 means "unknown", and two unknowns are not known to be
            // the same device, so each gets its own group.
            None => groups.push((dev, vec![i])),
        }
    }

    let mut out: Vec<Option<Result<StatMap>>> = (0..roots.len()).map(|_| None).collect();
    std::thread::scope(|scope| {
        let handles: Vec<_> = groups
            .iter()
            .map(|(_, idx)| {
                scope.spawn(move || {
                    idx.iter()
                        .map(|&i| (i, scan_directory(&roots[i], opt)))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        for h in handles {
            // A worker thread panicking must not lose the other groups'
            // results, so the failure is reported per root instead.
            match h.join() {
                Ok(results) => {
                    for (i, r) in results {
                        out[i] = Some(r);
                    }
                }
                Err(_) => return,
            }
        }
    });

    roots
        .iter()
        .cloned()
        .zip(out)
        .map(|(r, res)| {
            let res = res.unwrap_or_else(|| Err(anyhow!("scan thread for {} failed", r.display())));
            (r, res)
        })
        .collect()
}

pub fn scan_directory(root: impl AsRef<Path>, opt: &Options) -> Result<StatMap> {
    let scanner = Arc::new(crate::scanner::platform_scanner());
    scan_directory_with(root, opt, scanner)
}

/// Prepare shared state for a scan: compiled options and a scheduler seeded with the root.
fn prepare_scan(
    root: &Path,
    opt: &Options,
    threads: usize,
) -> (Arc<Options>, Vec<Worker<Job>>, Arc<Scheduler>) {
    let mut compiled = opt.clone();
    compile_filters_in_place(&mut compiled);
    // Following links can revisit a directory forever (symlink or junction
    // pointing at an ancestor). Cycle detection is mandatory, not opt-in.
    if compiled.follow_links && compiled.visited_dirs.is_none() {
        compiled.visited_dirs = Some(Arc::new(DashMap::with_capacity(1024)));
    }
    // `count_hardlinks == false` means "count a hardlinked file once", which
    // needs the inode map. Leaving it unallocated made the dedupe silently do
    // nothing, so a tree with hardlinks reported more bytes than `du`.
    if !compiled.count_hardlinks && compiled.inode_cache.is_none() {
        compiled.inode_cache = Some(Arc::new(DashMap::with_capacity(1024)));
    }
    if compiled.one_file_system {
        compiled.root_fs_id = platform::filesystem_id(root);
    }
    // Readahead is off unless asked for: it shortens latency by reading more
    // than the scan needs, which is the opposite of what a background scan
    // wants. Throughput opts in, an explicit setting always wins.
    compiled.io_prefetch = compiled
        .prefetch
        .unwrap_or(matches!(compiled.io_profile, IoProfile::Throughput));
    if compiled.io_profile == IoProfile::Gentle {
        // Gentle trades wall-clock for staying out of the way: few workers, and
        // no re-opening a large directory from a second worker.
        compiled.dir_yield_every.store(0, Ordering::Relaxed);
    }
    // Report the worker count that actually runs, not the one that was asked
    // for: the profile may have capped it, and the runtime throttle must not
    // be allowed to re-raise it past that cap.
    compiled.threads = threads;
    compiled.active_threads = Arc::new(AtomicUsize::new(threads));
    let workers = Scheduler::make_workers(threads);
    let sched = Arc::new(Scheduler::new(&workers));
    sched.push_high(Job {
        dir: root.to_path_buf(),
        depth: 0,
        resume: None,
    });
    (Arc::new(compiled), workers, sched)
}

/// Releases one job from the scheduler's in-flight count, including on unwind.
struct FinishOnDrop<'a>(&'a Scheduler);

impl Drop for FinishOnDrop<'_> {
    fn drop(&mut self) {
        self.0.finish_job();
    }
}

/// Worker loop shared by the thread-based and rayon-based schedulers.
fn run_worker(
    index: usize,
    local: Worker<Job>,
    sched: &Scheduler,
    options: &Options,
    total_files: &AtomicU64,
    scanner: &dyn FileSystemScanner,
) -> StatMap {
    #[cfg(any(feature = "prof-tracy", feature = "prof-puffin"))]
    profiling::register_thread!();
    let mut local_map: StatMap = HashMap::default();
    let mut next = index;
    let backoff = Backoff::new();
    loop {
        if options.cancel.load(Ordering::Relaxed) || sched.is_finished() {
            break;
        }
        // Runtime thread throttling: only the first `active_threads` workers take jobs.
        if index >= options.active_threads.load(Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_millis(5));
            continue;
        }
        let Some(Job { dir, depth, resume }) = sched.find_job(&local, &mut next) else {
            if !sched.wait_for_work(&backoff) {
                break;
            }
            continue;
        };
        backoff.reset();
        #[cfg(any(feature = "prof-tracy", feature = "prof-puffin"))]
        profiling::scope!("process_dir_loop");
        let ctx = ScanContext {
            options,
            sched,
            local: &local,
            total_files,
        };
        let dctx = DirContext {
            dir: &dir,
            depth,
            resume,
        };
        // The counter must come back down even if process_dir unwinds, or the
        // remaining workers would wait for a job that will never finish.
        let done = FinishOnDrop(sched);
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            scanner.process_dir(&ctx, &dctx, &mut local_map);
        }));
        drop(done);
        if outcome.is_err() {
            // This worker is leaving and its deque goes with it. Release the
            // jobs still queued there, or the in-flight count would never reach
            // zero and the remaining workers would spin forever.
            crate::error_handling::report_error(options, &dir, "scan worker panicked");
            while local.pop().is_some() {
                sched.finish_job();
            }
            break;
        }
    }
    local_map
}

fn merge_into(acc: &mut StatMap, part: StatMap) {
    if acc.is_empty() {
        *acc = part;
        return;
    }
    for (k, v) in part {
        acc.entry(k).or_default().add(&v);
    }
}

/// CPU count for thread pinning, or 0 when pinning is off.
///
/// Resolved once per process. `env::var` takes a global lock and walks the
/// environment, and this used to run on every worker as it started, which is
/// pure startup cost on a scan that never pins.
#[cfg(target_os = "linux")]
fn pin_cpu_count() -> i64 {
    use std::sync::OnceLock;
    static N: OnceLock<i64> = OnceLock::new();
    *N.get_or_init(|| {
        if std::env::var("HYPERDU_PIN_THREADS").ok().as_deref() != Some("1") {
            return 0;
        }
        unsafe { libc::sysconf(libc::_SC_NPROCESSORS_ONLN) }.max(0)
    })
}

#[cfg(target_os = "linux")]
fn pin_thread_if_requested(i: usize) {
    let ncpu = pin_cpu_count();
    if ncpu == 0 {
        return;
    }
    // Pin this worker to a CPU id based on index
    unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        let cpu = if ncpu > 0 {
            (i as i64 % ncpu) as usize
        } else {
            i
        };
        libc::CPU_ZERO(&mut set);
        libc::CPU_SET(cpu, &mut set);
        let _ = libc::sched_setaffinity(
            0,
            std::mem::size_of::<libc::cpu_set_t>(),
            &set as *const libc::cpu_set_t,
        );
    }
}

/// Variant of scan_directory that accepts a custom scanner implementation.
/// Useful for unit tests and alternative backends.
pub fn scan_directory_with(
    root: impl AsRef<Path>,
    opt: &Options,
    scanner: Arc<dyn FileSystemScanner>,
) -> Result<StatMap> {
    #[cfg(any(feature = "prof-tracy", feature = "prof-puffin"))]
    profiling::scope!("scan_directory");
    let root = root.as_ref().to_path_buf();
    if !root.exists() {
        return Err(anyhow!("root does not exist: {}", root.display()));
    }

    let threads = effective_threads(opt);
    let total_files = Arc::new(AtomicU64::new(0));
    let (options, workers, sched) = prepare_scan(&root, opt, threads);

    // Start adaptive tuner if enabled
    let _tuner = tuning::start_if_enabled(options.clone(), total_files.clone());

    let mut handles = Vec::with_capacity(threads);
    for (i, local) in workers.into_iter().enumerate() {
        let sched = sched.clone();
        let options = options.clone();
        let total_files = total_files.clone();
        let scanner = scanner.clone();
        let handle = std::thread::Builder::new()
            .name(format!("hyperdu-w{i}"))
            .spawn(move || {
                #[cfg(target_os = "linux")]
                pin_thread_if_requested(i);
                run_worker(i, local, &sched, &options, &total_files, scanner.as_ref())
            })
            .map_err(|e| anyhow!("failed to spawn worker thread: {e}"))?;
        handles.push(handle);
    }

    // Merge thread maps
    let mut merged: StatMap = HashMap::default();
    for h in handles {
        merge_into(&mut merged, h.join().unwrap_or_default());
    }

    Ok(rollup::rollup_child_to_parent(merged))
}

/// Experimental rayon-based internal scheduler. Uses a rayon thread-pool with `opt.threads`
/// threads and runs worker loops as rayon tasks instead of OS threads.
#[cfg(feature = "rayon-inner")]
pub fn scan_directory_rayon(root: impl AsRef<Path>, opt: &Options) -> Result<StatMap> {
    use rayon::ThreadPoolBuilder;
    let scanner = Arc::new(crate::scanner::platform_scanner());
    let root = root.as_ref().to_path_buf();
    if !root.exists() {
        return Err(anyhow!("root does not exist: {}", root.display()));
    }
    let threads = effective_threads(opt);
    let total_files = Arc::new(AtomicU64::new(0));
    let (options, workers, sched) = prepare_scan(&root, opt, threads);
    let merged = Arc::new(std::sync::Mutex::new(HashMap::default()));
    let pool = ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .map_err(|e| anyhow!("failed to build rayon pool: {e}"))?;
    pool.install(|| {
        rayon::scope(|s| {
            for (i, local) in workers.into_iter().enumerate() {
                let sched = sched.clone();
                let options = options.clone();
                let total_files = total_files.clone();
                let merged = merged.clone();
                let scanner = scanner.clone();
                s.spawn(move |_| {
                    let part =
                        run_worker(i, local, &sched, &options, &total_files, scanner.as_ref());
                    let mut g = merged.lock().unwrap_or_else(|e| e.into_inner());
                    merge_into(&mut g, part);
                });
            }
        });
    });
    let merged = std::mem::take(&mut *merged.lock().unwrap_or_else(|e| e.into_inner()));
    Ok(rollup::rollup_child_to_parent(merged))
}

#[cfg(not(windows))]
#[inline(always)]
fn name_contains_patterns_bytes(name: &[u8], patterns: &[String]) -> bool {
    if patterns.is_empty() {
        return false;
    }
    for pat in patterns {
        let pb = pat.as_bytes();
        if pb.is_empty() {
            continue;
        }
        if memchr::memmem::find(name, pb).is_some() {
            return true;
        }
    }
    false
}

#[cfg(not(windows))]
#[inline(always)]
pub(crate) fn name_matches(name: &[u8], opt: &Options) -> bool {
    // The automaton is built from exactly `exclude_contains`, so a miss already
    // proves none of those patterns occur. Re-scanning with memmem afterwards
    // doubled the per-entry substring work for every name that is kept, which is
    // almost all of them.
    match &opt.exclude_ac {
        Some(ac) => {
            if ac.is_match(name) {
                return true;
            }
        }
        None => {
            if name_contains_patterns_bytes(name, &opt.exclude_contains) {
                return true;
            }
        }
    }
    if let Some(rs) = &opt.exclude_regex_set {
        if let Ok(s) = std::str::from_utf8(name) {
            if rs.is_match(s) {
                return true;
            }
        }
    }
    false
}

/// Name-level exclusion on UTF-16 names (Windows backends). No allocation.
#[cfg(windows)]
#[inline(always)]
pub(crate) fn wname_matches(name: &[u16], opt: &Options) -> bool {
    opt.exclude_contains_w.iter().any(|pat| {
        !pat.is_empty() && pat.len() <= name.len() && name.windows(pat.len()).any(|w| w == &pat[..])
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compile_filters_sets_needs_path_filter() {
        let mut opt = Options {
            exclude_contains: vec!["a".into()],
            ..Options::default()
        };
        compile_filters_in_place(&mut opt);
        assert!(!opt.needs_path_filter);

        opt.exclude_contains = vec!["a/b".into()];
        compile_filters_in_place(&mut opt);
        assert!(opt.needs_path_filter);

        opt.exclude_contains = vec![];
        opt.exclude_glob = vec!["**/x/**".into()];
        compile_filters_in_place(&mut opt);
        assert!(opt.needs_path_filter);

        opt.exclude_glob = vec![];
        opt.exclude_regex = vec![".*tmp$".into()];
        compile_filters_in_place(&mut opt);
        assert!(opt.needs_path_filter);
    }

    #[cfg(windows)]
    #[test]
    fn wname_matches_utf16_patterns() {
        let mut opt = Options {
            exclude_contains: vec!["node_modules".into(), "".into()],
            ..Options::default()
        };
        compile_filters_in_place(&mut opt);
        let hit: Vec<u16> = "my_node_modules_x".encode_utf16().collect();
        let miss: Vec<u16> = "node".encode_utf16().collect();
        assert!(wname_matches(&hit, &opt));
        assert!(!wname_matches(&miss, &opt));
    }
}
