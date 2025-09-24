#[cfg(feature = "mimalloc")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize},
        Arc,
    },
};

use ahash::AHashMap as HashMap;
use aho_corasick::AhoCorasick;
use anyhow::{anyhow, Result};
use crossbeam_deque::{Injector, Steal, Worker};
use dashmap::DashMap;
use globset::{Glob, GlobSet, GlobSetBuilder};
use regex::RegexSet;
use serde::Serialize;

pub mod classify;
mod common_ops;
mod error_handling;
mod filters; // centralize filter helpers
pub mod fs_strategy;
pub mod incremental;
pub mod memory_pool;
mod options; // for OptionsBuilder
mod platform;
mod rollup;
mod scanner; // FileSystemScanner + platform default
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompatMode {
    HyperDU,
    GnuBasic,
    GnuStrict,
    PosixStrict,
}

#[derive(Default, Clone, Copy, Serialize, Debug)]
pub struct Stat {
    pub logical: u64,
    pub physical: u64,
    pub files: u64,
}

#[derive(Clone)]
pub struct Options {
    pub exclude_contains: Vec<String>,
    pub max_depth: u32,     // 0 = unlimited
    pub min_file_size: u64, // bytes
    pub follow_links: bool,
    pub threads: usize,
    pub progress_every: u64, // 0 = disabled
    pub progress_callback: Option<Arc<dyn Fn(u64) + Send + Sync + 'static>>, // called with file count
    pub progress_path_callback: Option<Arc<dyn Fn(&Path) + Send + Sync + 'static>>, // occasionally called with a sample file path
    pub compute_physical: bool, // if false, use logical size as physical (faster)
    pub dir_yield_every: Arc<AtomicUsize>, // 0 = no yielding; split large dirs every N entries
    pub approximate_sizes: bool, // if true and compute_physical=false, estimate regular file size (e.g., 4KiB) to avoid statx
    pub disable_uring: bool,     // if true, force-disable io_uring backend even if compiled
    pub active_threads: Arc<AtomicUsize>, // runtime-tunable active worker threads (<= threads)
    pub uring_batch: Arc<AtomicUsize>, // dynamic batch size for io_uring statx (Linux only); default 128
    pub uring_sq_depth: Arc<AtomicUsize>, // io_uring SQ/CQ depth (Linux only); default 256
    pub uring_sqe_fail: Arc<AtomicU64>, // number of SQE push failures (queue full)
    pub uring_submit_wait_ns: Arc<AtomicU64>, // accumulated submit_and_wait time (ns)
    pub uring_sqe_enq: Arc<AtomicU64>, // enqueued SQEs
    pub uring_cqe_comp: Arc<AtomicU64>, // completed CQEs
    pub uring_cqe_err: Arc<AtomicU64>, // CQE errors (<0 result)
    pub cancel: Arc<AtomicBool>,       // cooperative cancellation
    pub exclude_ac: Option<AhoCorasick>,
    pub exclude_regex: Vec<String>,
    pub exclude_glob: Vec<String>,
    pub exclude_regex_set: Option<RegexSet>,
    pub exclude_glob_set: Option<GlobSet>,
    // Compatibility and correctness knobs
    pub compat_mode: CompatMode,
    pub count_hardlinks: bool, // if true, count hardlinks as separate (non-GNU). Default false = dedupe hardlinks like GNU du
    pub inode_cache: Option<Arc<DashMap<(u64, u64), ()>>>, // (dev, ino)
    pub error_count: Arc<AtomicU64>,
    pub error_report: Option<Arc<dyn Fn(&str) + Send + Sync + 'static>>, // optional error reporter
    pub one_file_system: bool,
    pub visited_bloom: Option<Arc<Bloom>>, // fast pre-check
    pub visited_dirs: Option<Arc<DashMap<(u64, u64), ()>>>, // loop detection when following links
    // Keep progress lightweight: we intentionally do not accumulate sizes per-file here.
    // Adaptive tuning / scheduling preferences (configured by CLI config)
    pub tune_enabled: bool,
    pub tune_interval_ms: u64,
    pub heuristics_mode: HeuristicsMode,
    pub prefer_inner_rayon: bool,
    // Windows-specific tuning knobs
    pub win_allow_handle: bool,
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
        Self {
            exclude_contains: vec![".git".into(), "node_modules".into(), "target".into()],
            max_depth: 0,
            min_file_size: 0,
            follow_links: false,
            threads: std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4),
            progress_every: 0,
            progress_callback: None,
            progress_path_callback: None,
            compute_physical: true,
            dir_yield_every: Arc::new(AtomicUsize::new(
                std::env::var("HYPERDU_DIR_YIELD_EVERY")
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0),
            )),
            approximate_sizes: false,
            disable_uring: std::env::var("HYPERDU_DISABLE_URING")
                .ok()
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false),
            active_threads: Arc::new(AtomicUsize::new(0)),
            uring_batch: Arc::new(AtomicUsize::new(
                std::env::var("HYPERDU_STATX_BATCH")
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(128),
            )),
            uring_sq_depth: Arc::new(AtomicUsize::new(
                std::env::var("HYPERDU_URING_SQ_DEPTH")
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(256),
            )),
            uring_sqe_fail: Arc::new(AtomicU64::new(0)),
            uring_submit_wait_ns: Arc::new(AtomicU64::new(0)),
            uring_sqe_enq: Arc::new(AtomicU64::new(0)),
            uring_cqe_comp: Arc::new(AtomicU64::new(0)),
            uring_cqe_err: Arc::new(AtomicU64::new(0)),
            exclude_ac: None,
            exclude_regex: Vec::new(),
            exclude_glob: Vec::new(),
            exclude_regex_set: None,
            exclude_glob_set: None,
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
    bits: Box<[std::sync::atomic::AtomicU64]>,
}
impl Bloom {
    pub fn with_bits(n_bits: usize) -> Self {
        let n = n_bits.next_power_of_two().max(1 << 20); // at least ~1M bits
        let words = n.div_ceil(64);
        let mut v: Vec<std::sync::atomic::AtomicU64> = Vec::with_capacity(words);
        v.resize_with(words, || std::sync::atomic::AtomicU64::new(0));
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
        let old1 = w1.fetch_or(b1, std::sync::atomic::Ordering::Relaxed);
        let old2 = w2.fetch_or(b2, std::sync::atomic::Ordering::Relaxed);
        (old1 & b1 != 0) & (old2 & b2 != 0)
    }
}

pub type StatMap = HashMap<PathBuf, Stat>;

// Lightweight, zero-cost wrappers to reduce parameter explosion in hot calls.
// These are constructed at call-sites and are expected to be fully inlined.
#[derive(Clone, Copy)]
pub struct ScanContext<'a> {
    pub(crate) options: &'a Options,
    pub(crate) high_injector: &'a Injector<Job>,
    pub(crate) normal_injector: &'a Injector<Job>,
    pub(crate) total_files: &'a std::sync::atomic::AtomicU64,
}

#[derive(Clone, Copy)]
pub struct DirContext<'a> {
    pub dir: &'a Path,
    pub depth: u32,
    pub resume: Option<u64>,
}

impl<'a> ScanContext<'a> {
    #[inline]
    pub fn enqueue_dir(&self, path: PathBuf, depth: u32) {
        self.normal_injector.push(Job {
            dir: path,
            depth,
            resume: None,
        });
    }

    #[inline]
    pub fn enqueue_resume(&self, path: PathBuf, depth: u32, resume: u64) {
        self.high_injector.push(Job {
            dir: path,
            depth,
            resume: Some(resume),
        });
    }

    #[inline]
    pub fn report_progress(&self, opt: &Options, path: Option<&Path>) {
        crate::common_ops::report_file_progress(opt, self.total_files, path);
    }
}

#[inline]
fn compile_filters_in_place(opt: &mut Options) {
    if !opt.exclude_contains.is_empty() {
        let pats: Vec<&str> = opt.exclude_contains.iter().map(|s| s.as_str()).collect();
        opt.exclude_ac = AhoCorasick::new(&pats).ok();
    } else {
        opt.exclude_ac = None;
    }
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
}

#[derive(Clone, Debug)]
struct Job {
    dir: PathBuf,
    depth: u32,
    resume: Option<u64>,
}

pub fn scan_directory(root: impl AsRef<Path>, opt: &Options) -> Result<StatMap> {
    let scanner = Arc::new(crate::scanner::platform_scanner());
    scan_directory_with(root, opt, scanner)
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

    let threads = opt.threads.max(1);
    let high_injector: Arc<Injector<Job>> = Arc::new(Injector::new());
    let normal_injector: Arc<Injector<Job>> = Arc::new(Injector::new());
    high_injector.push(Job {
        dir: root.clone(),
        depth: 0,
        resume: None,
    });

    let total_files = Arc::new(AtomicU64::new(0));

    let workers: Vec<Worker<Job>> = (0..threads).map(|_| Worker::new_fifo()).collect();
    let stealers = workers.iter().map(|w| w.stealer()).collect::<Vec<_>>();

    let mut compiled = opt.clone();
    compile_filters_in_place(&mut compiled);
    let options = Arc::new(compiled);

    // Start adaptive tuner if enabled
    let _tuner = tuning::start_if_enabled(options.clone(), total_files.clone());

    let mut handles = Vec::with_capacity(threads);
    for (i, local) in workers.into_iter().enumerate() {
        let high_ref = high_injector.clone();
        let normal_ref = normal_injector.clone();
        let stealers_ref = stealers.clone();
        let options = options.clone();
        let total_files = total_files.clone();
        let scanner = scanner.clone();
        let handle = std::thread::spawn(move || {
            #[cfg(target_os = "linux")]
            {
                if std::env::var("HYPERDU_PIN_THREADS").ok().as_deref() == Some("1") {
                    // Pin this worker to a CPU id based on index
                    unsafe {
                        let mut set: libc::cpu_set_t = std::mem::zeroed();
                        let ncpu = libc::sysconf(libc::_SC_NPROCESSORS_ONLN);
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
            }
            #[cfg(any(feature = "prof-tracy", feature = "prof-puffin"))]
            profiling::register_thread!();
            let mut local_map: StatMap = HashMap::default();
            let mut next = i % stealers_ref.len().max(1);
            loop {
                if options.cancel.load(std::sync::atomic::Ordering::Relaxed) {
                    break;
                }
                // Runtime thread throttling: only first `active_threads` workers fetch jobs
                let act = options
                    .active_threads
                    .load(std::sync::atomic::Ordering::Relaxed);
                if i >= act {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                    continue;
                }
                let job = local.pop().or_else(|| match high_ref.steal() {
                    Steal::Success(j) => Some(j),
                    Steal::Empty => match normal_ref.steal() {
                        Steal::Success(j) => Some(j),
                        Steal::Empty => {
                            let mut found = None;
                            let len = stealers_ref.len();
                            for k in 0..len {
                                let idx = (next + k) % len;
                                match stealers_ref[idx].steal() {
                                    Steal::Success(j) => {
                                        found = Some(j);
                                        break;
                                    }
                                    Steal::Retry => {}
                                    Steal::Empty => {}
                                }
                            }
                            if len > 0 {
                                next = (next + 1) % len;
                            }
                            found
                        }
                        Steal::Retry => None,
                    },
                    Steal::Retry => None,
                });

                let Some(Job { dir, depth, resume }) = job else {
                    break;
                };
                if path_excluded(&dir, &options) {
                    continue;
                }
                #[cfg(any(feature = "prof-tracy", feature = "prof-puffin"))]
                profiling::scope!("process_dir_loop");
                let ctx = ScanContext {
                    options: &options,
                    high_injector: &high_ref,
                    normal_injector: &normal_ref,
                    total_files: &total_files,
                };
                let dctx = DirContext {
                    dir: &dir,
                    depth,
                    resume,
                };
                scanner.process_dir(&ctx, &dctx, &mut local_map);
            }
            local_map
        });
        handles.push(handle);
    }

    // Merge thread maps
    let mut merged: StatMap = HashMap::default();
    for h in handles {
        for (k, v) in h.join().unwrap_or_default() {
            let e = merged.entry(k).or_default();
            e.logical += v.logical;
            e.physical += v.physical;
            e.files += v.files;
        }
    }

    let merged = rollup::rollup_child_to_parent(merged);
    Ok(merged)
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
    let threads = opt.threads.max(1);
    let high_injector: Arc<Injector<Job>> = Arc::new(Injector::new());
    let normal_injector: Arc<Injector<Job>> = Arc::new(Injector::new());
    high_injector.push(Job {
        dir: root.clone(),
        depth: 0,
        resume: None,
    });
    let total_files = Arc::new(AtomicU64::new(0));
    let mut compiled = opt.clone();
    compile_filters_in_place(&mut compiled);
    let options = Arc::new(compiled);
    let workers: Vec<Worker<Job>> = (0..threads).map(|_| Worker::new_fifo()).collect();
    let stealers = workers.iter().map(|w| w.stealer()).collect::<Vec<_>>();
    let merged = Arc::new(std::sync::Mutex::new(HashMap::default()));
    let pool = ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .unwrap();
    pool.install(|| {
        rayon::scope(|s| {
            for (i, local) in workers.into_iter().enumerate() {
                let high_ref = high_injector.clone();
                let normal_ref = normal_injector.clone();
                let stealers_ref = stealers.clone();
                let options = options.clone();
                let total_files = total_files.clone();
                let merged = merged.clone();
                let scanner2 = scanner.clone();
                s.spawn(move |_| {
                    let mut local_map: StatMap = HashMap::default();
                    let mut next = i % stealers_ref.len().max(1);
                    loop {
                        if options.cancel.load(std::sync::atomic::Ordering::Relaxed) {
                            break;
                        }
                        let job = local.pop().or_else(|| match high_ref.steal() {
                            Steal::Success(j) => Some(j),
                            Steal::Empty => match normal_ref.steal() {
                                Steal::Success(j) => Some(j),
                                Steal::Empty => {
                                    let mut found = None;
                                    let len = stealers_ref.len();
                                    for k in 0..len {
                                        let idx = (next + k) % len;
                                        match stealers_ref[idx].steal() {
                                            Steal::Success(j) => {
                                                found = Some(j);
                                                break;
                                            }
                                            Steal::Retry => {}
                                            Steal::Empty => {}
                                        }
                                    }
                                    if len > 0 {
                                        next = (next + 1) % len;
                                    }
                                    found
                                }
                                Steal::Retry => None,
                            },
                            Steal::Retry => None,
                        });
                        let Some(Job { dir, depth, resume }) = job else {
                            break;
                        };
                        if path_excluded(&dir, &options) {
                            continue;
                        }
                        let ctx = ScanContext {
                            options: &options,
                            high_injector: &high_ref,
                            normal_injector: &normal_ref,
                            total_files: &total_files,
                        };
                        let dctx = DirContext {
                            dir: &dir,
                            depth,
                            resume,
                        };
                        scanner2.process_dir(&ctx, &dctx, &mut local_map);
                    }
                    let mut g = merged.lock().unwrap();
                    for (k, v) in local_map {
                        let e: &mut Stat = g.entry(k).or_default();
                        e.logical += v.logical;
                        e.physical += v.physical;
                        e.files += v.files;
                    }
                });
            }
        });
    });
    let merged = std::mem::take(&mut *merged.lock().unwrap());
    let merged = rollup::rollup_child_to_parent(merged);
    Ok(merged)
}

use crate::filters::path_excluded;

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
    if let Some(ac) = &opt.exclude_ac {
        if ac.is_match(name) {
            return true;
        }
    }
    if let Some(rs) = &opt.exclude_regex_set {
        if let Ok(s) = std::str::from_utf8(name) {
            if rs.is_match(s) {
                return true;
            }
        }
    }
    name_contains_patterns_bytes(name, &opt.exclude_contains)
}

#[cfg(windows)]
#[inline(always)]
fn wname_contains_patterns_lossy(name: &std::ffi::OsString, patterns: &[String]) -> bool {
    if patterns.is_empty() {
        return false;
    }
    let s = name.to_string_lossy();
    patterns.iter().any(|q| !q.is_empty() && s.contains(q))
}

#[cfg(windows)]
#[cfg(any())]
fn process_dir(
    dir: &Path,
    depth: u32,
    opt: &Options,
    map: &mut StatMap,
    injector: &Injector<Job>,
    total_files: &AtomicU64,
) {
    use std::{ffi::OsString, os::windows::ffi::OsStrExt};

    use windows::{
        core::PCWSTR,
        Win32::Storage::FileSystem::{
            FindClose, FindExInfoBasic, FindExSearchNameMatch, FindFirstFileExW, FindNextFileW,
            GetCompressedFileSizeW, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
            FIND_FIRST_EX_LARGE_FETCH, WIN32_FIND_DATAW,
        },
    };

    // Build wide pattern: <dir>\*\0
    #[cfg(any(feature = "prof-tracy", feature = "prof-puffin"))]
    profiling::scope!("win32_find_first");
    let mut pattern: Vec<u16> = dir.as_os_str().encode_wide().collect();
    let last = pattern.last().copied();
    if last != Some(92) && last != Some(47) {
        pattern.push(92);
    }
    pattern.push('*' as u16);
    pattern.push(0);

    unsafe {
        let mut data: WIN32_FIND_DATAW = std::mem::zeroed();
        let handle = FindFirstFileExW(
            PCWSTR(pattern.as_ptr()),
            FindExInfoBasic,
            &mut data as *mut _ as *mut _,
            FindExSearchNameMatch,
            None,
            FIND_FIRST_EX_LARGE_FETCH,
        );
        if handle.is_invalid() {
            return;
        }

        let mut first = true;
        loop {
            if !first {
                let ok = FindNextFileW(handle, &mut data).as_bool();
                if !ok {
                    break;
                }
            }
            first = false;

            // Name
            let name_len = (0..).take_while(|&i| data.cFileName[i] != 0).count();
            let name = OsString::from_wide(&data.cFileName[..name_len]);
            if name == OsString::from(".") || name == OsString::from("..") {
                continue;
            }

            let is_dir = (data.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY).0 != 0;
            let is_reparse = (data.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT).0 != 0;
            if is_reparse && !opt.follow_links {
                continue;
            }

            // Fast name-only exclude to avoid full join on common patterns
            if wname_contains_patterns_lossy(&name, &opt.exclude_contains) {
                continue;
            }
            let child = dir.join(&name);
            if should_exclude(&child, &opt.exclude_contains) {
                continue;
            }

            if is_dir {
                if opt.max_depth == 0 || depth < opt.max_depth {
                    injector.push(Job {
                        dir: child,
                        depth: depth + 1,
                    });
                }
            } else {
                let logical = ((data.nFileSizeHigh as u64) << 32) | (data.nFileSizeLow as u64);
                if logical >= opt.min_file_size {
                    #[cfg(any(feature = "prof-tracy", feature = "prof-puffin"))]
                    profiling::scope!("GetCompressedFileSizeW");
                    let mut physical = logical;
                    // GetCompressedFileSizeW requires path
                    let wide: Vec<u16> = child
                        .as_os_str()
                        .encode_wide()
                        .chain(std::iter::once(0))
                        .collect();
                    let mut high: u32 = 0;
                    let low = GetCompressedFileSizeW(PCWSTR(wide.as_ptr()), Some(&mut high));
                    let combined = ((high as u64) << 32) | (low as u64);
                    if low != u32::MAX {
                        physical = combined;
                    }
                    let e = map.entry(dir.to_path_buf()).or_default();
                    e.logical += logical;
                    e.physical += physical;
                    e.files += 1;
                    if opt.progress_every > 0 {
                        let n = total_files.fetch_add(1, Ordering::Relaxed) + 1;
                        if n % opt.progress_every == 0 {
                            if let Some(cb) = &opt.progress_callback {
                                cb(n);
                            }
                        }
                    }
                }
            }
        }
        let _ = FindClose(handle);
    }
}

#[cfg(target_os = "macos")]
#[cfg(any())]
fn process_dir(
    dir: &Path,
    depth: u32,
    opt: &Options,
    map: &mut StatMap,
    injector: &Injector<Job>,
    total_files: &AtomicU64,
) {
    use std::{
        ffi::{CString, OsStr},
        os::unix::ffi::OsStrExt,
    };

    // macOS: readdir + lstat
    let c_path = CString::new(dir.as_os_str().as_bytes()).ok();
    let Some(c_path) = c_path else { return };
    let fd = unsafe {
        libc::open(
            c_path.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return;
    }
    let d = unsafe { libc::fdopendir(fd) };
    if d.is_null() {
        unsafe { libc::close(fd) };
        return;
    }

    loop {
        unsafe {
            libc::errno = 0;
        }
        let ent = unsafe { libc::readdir(d) };
        if ent.is_null() {
            break;
        }
        let entry = unsafe { &*ent };
        if entry.d_name[0] == 0 {
            continue;
        }
        let name_c = unsafe { std::ffi::CStr::from_ptr(entry.d_name.as_ptr()) };
        let name_b = name_c.to_bytes();
        if name_b == b"." || name_b == b".." {
            continue;
        }
        if name_contains_patterns_bytes(name_b, &opt.exclude_contains) {
            continue;
        }
        let child = dir.join(OsStr::from_bytes(name_b));
        if should_exclude(&child, &opt.exclude_contains) {
            continue;
        }

        // macOS dirent doesn't reliably set d_type; use lstat to detect type/size
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        let c_child = match CString::new(child.as_os_str().as_bytes()) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let rc = unsafe { libc::lstat(c_child.as_ptr(), &mut st) };
        if rc != 0 {
            continue;
        }
        let mode = st.st_mode as u32;
        let is_dir = (mode & libc::S_IFMT) == libc::S_IFDIR;
        let is_lnk = (mode & libc::S_IFMT) == libc::S_IFLNK;
        if is_lnk && !opt.follow_links {
            continue;
        }
        if is_dir {
            if opt.max_depth == 0 || depth < opt.max_depth {
                injector.push(Job {
                    dir: child,
                    depth: depth + 1,
                    resume: None,
                });
            }
        } else {
            let logical = st.st_size as u64;
            if logical >= opt.min_file_size {
                let physical_raw = (st.st_blocks as u64) * 512u64;
                let physical = if physical_raw == 0 {
                    logical
                } else {
                    physical_raw
                };
                let e = map.entry(dir.to_path_buf()).or_default();
                e.logical += logical;
                e.physical += physical;
                e.files += 1;
                if opt.progress_every > 0 {
                    let n = total_files.fetch_add(1, Ordering::Relaxed) + 1;
                    if n % opt.progress_every == 0 {
                        if let Some(cb) = &opt.progress_callback {
                            cb(n);
                        }
                    }
                }
            }
        }
    }
    unsafe { libc::closedir(d) };
}

#[cfg(all(
    unix,
    not(target_os = "macos"),
    not(all(target_os = "linux", target_arch = "x86_64"))
))]
#[cfg(any())]
fn process_dir(
    dir: &Path,
    depth: u32,
    opt: &Options,
    map: &mut StatMap,
    injector: &Injector<Job>,
    total_files: &AtomicU64,
) {
    use std::{
        ffi::{CString, OsStr},
        os::unix::ffi::OsStrExt,
    };

    // Open directory via opendir
    let c_path = CString::new(dir.as_os_str().as_bytes()).ok();
    let Some(c_path) = c_path else { return };
    let fd = unsafe {
        libc::open(
            c_path.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return;
    }
    let d = unsafe { libc::fdopendir(fd) };
    if d.is_null() {
        unsafe { libc::close(fd) };
        return;
    }

    loop {
        #[cfg(any(feature = "prof-tracy", feature = "prof-puffin"))]
        profiling::scope!("readdir_loop");
        unsafe {
            libc::errno = 0;
        }
        let ent = unsafe { libc::readdir(d) };
        if ent.is_null() {
            break;
        }
        let entry = unsafe { &*ent };
        if entry.d_name[0] == 0 {
            continue;
        }
        let name_c = unsafe { std::ffi::CStr::from_ptr(entry.d_name.as_ptr()) };
        let name_b = name_c.to_bytes();
        if name_b == b"." || name_b == b".." {
            continue;
        }
        if name_contains_patterns_bytes(name_b, &opt.exclude_contains) {
            continue;
        }
        let child = dir.join(OsStr::from_bytes(name_b));
        if should_exclude(&child, &opt.exclude_contains) {
            continue;
        }

        let dtype = entry.d_type as i32;
        let is_dir = dtype == libc::DT_DIR;
        let is_lnk = dtype == libc::DT_LNK;

        if is_lnk && !opt.follow_links {
            continue;
        }

        if is_dir {
            if opt.max_depth == 0 || depth < opt.max_depth {
                injector.push(Job {
                    dir: child,
                    depth: depth + 1,
                });
            }
        } else {
            let mut stx: libc::statx = unsafe { std::mem::zeroed() };
            let c_child = CString::new(child.as_os_str().as_bytes()).ok();
            if let Some(c_child) = c_child {
                let flags = if opt.follow_links {
                    0
                } else {
                    libc::AT_SYMLINK_NOFOLLOW
                };
                #[cfg(any(feature = "prof-tracy", feature = "prof-puffin"))]
                profiling::scope!("statx_fallback");
                let rc = unsafe {
                    libc::statx(
                        libc::AT_FDCWD,
                        c_child.as_ptr(),
                        flags,
                        libc::STATX_SIZE | libc::STATX_BLOCKS,
                        &mut stx,
                    )
                };
                if rc == 0 {
                    let logical = stx.stx_size as u64;
                    if logical >= opt.min_file_size {
                        let physical = (stx.stx_blocks as u64) * 512u64;
                        let e = map.entry(dir.to_path_buf()).or_default();
                        e.logical += logical;
                        e.physical += if physical == 0 { logical } else { physical };
                        e.files += 1;
                        if opt.progress_every > 0 {
                            let n = total_files.fetch_add(1, Ordering::Relaxed) + 1;
                            if n % opt.progress_every == 0 {
                                if let Some(cb) = &opt.progress_callback {
                                    cb(n);
                                }
                            }
                        }
                    }
                } else if let Ok(md) = std::fs::symlink_metadata(&child) {
                    if md.file_type().is_file() {
                        let logical = md.len();
                        if logical >= opt.min_file_size {
                            let e = map.entry(dir.to_path_buf()).or_default();
                            e.logical += logical;
                            e.physical += logical;
                            e.files += 1;
                            if opt.progress_every > 0 {
                                let n = total_files.fetch_add(1, Ordering::Relaxed) + 1;
                                if n % opt.progress_every == 0 {
                                    if let Some(cb) = &opt.progress_callback {
                                        cb(n);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    unsafe { libc::closedir(d) };
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[cfg(any())]
fn process_dir(
    dir: &Path,
    depth: u32,
    opt: &Options,
    map: &mut StatMap,
    injector: &Injector<Job>,
    total_files: &AtomicU64,
) {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};

    const SYS_GETDENTS64: libc::c_long = 217; // x86_64

    // Open directory
    let c_path = match CString::new(dir.as_os_str().as_bytes()) {
        Ok(s) => s,
        Err(_) => return,
    };
    let fd = unsafe {
        libc::open(
            c_path.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if fd < 0 {
        return;
    }

    const GETDENTS_BUF: usize = 64 * 1024; // tuneable: 64-256KB
    let mut buf = vec![0u8; GETDENTS_BUF];
    loop {
        #[cfg(any(feature = "prof-tracy", feature = "prof-puffin"))]
        profiling::scope!("getdents64_loop");
        let nread = unsafe {
            libc::syscall(
                SYS_GETDENTS64,
                fd,
                buf.as_mut_ptr() as *mut libc::c_void,
                buf.len(),
            )
        } as isize;
        if nread == -1 {
            break;
        }
        if nread == 0 {
            break;
        }
        let mut bpos: isize = 0;
        while bpos < nread {
            let ptr = unsafe { buf.as_ptr().offset(bpos) };
            // Layout: ino(8) off(8) reclen(2) type(1) name(\0-terminated)
            let d_reclen = unsafe { *(ptr.add(16) as *const u16) } as isize;
            let d_type = unsafe { *(ptr.add(18) as *const u8) };
            let name_ptr = unsafe { ptr.add(19) };
            // Find null terminator within record
            let mut name_len = 0usize;
            while (19 + name_len as isize) < d_reclen {
                let c = unsafe { *ptr.add((19 + name_len as isize) as usize) };
                if c == 0 {
                    break;
                }
                name_len += 1;
            }
            let name_slice = unsafe { std::slice::from_raw_parts(name_ptr, name_len) };
            if name_slice == b"." || name_slice == b".." {
                bpos += d_reclen;
                continue;
            }
            if name_contains_patterns_bytes(name_slice, &opt.exclude_contains) {
                bpos += d_reclen;
                continue;
            }

            let dtype = d_type;
            let is_dir_hint = dtype == libc::DT_DIR;
            let is_lnk = dtype == libc::DT_LNK;

            use std::ffi::OsStr;
            let child_path = dir.join(OsStr::from_bytes(name_slice));
            if should_exclude(&child_path, &opt.exclude_contains) {
                bpos += d_reclen;
                continue;
            }
            if is_lnk && !opt.follow_links {
                bpos += d_reclen;
                continue;
            }

            if is_dir_hint {
                if opt.max_depth == 0 || depth < opt.max_depth {
                    injector.push(Job {
                        dir: child_path,
                        depth: depth + 1,
                    });
                }
            } else {
                // statx relative to dir fd
                let mut stx: libc::statx = unsafe { std::mem::zeroed() };
                let c_name = match CString::new(name_slice) {
                    Ok(s) => s,
                    Err(_) => {
                        bpos += d_reclen;
                        continue;
                    }
                };
                let flags = if opt.follow_links {
                    0
                } else {
                    libc::AT_SYMLINK_NOFOLLOW
                };
                #[cfg(any(feature = "prof-tracy", feature = "prof-puffin"))]
                profiling::scope!("statx");
                let rc = unsafe {
                    libc::statx(
                        fd,
                        c_name.as_ptr(),
                        flags,
                        libc::STATX_SIZE | libc::STATX_BLOCKS | libc::STATX_MODE,
                        &mut stx,
                    )
                };
                if rc == 0 {
                    let mode = stx.stx_mode as u32;
                    let ftype = mode & libc::S_IFMT;
                    if ftype == libc::S_IFDIR {
                        if opt.max_depth == 0 || depth < opt.max_depth {
                            injector.push(Job {
                                dir: child_path,
                                depth: depth + 1,
                            });
                        }
                    } else if ftype == libc::S_IFREG || (opt.follow_links && ftype == libc::S_IFLNK)
                    {
                        let logical = stx.stx_size;
                        if logical >= opt.min_file_size {
                            let physical_raw = stx.stx_blocks * 512u64;
                            let physical = if physical_raw == 0 {
                                logical
                            } else {
                                physical_raw
                            };
                            let e = map.entry(dir.to_path_buf()).or_default();
                            e.logical += logical;
                            e.physical += physical;
                            e.files += 1;
                            if opt.progress_every > 0 {
                                let n = total_files.fetch_add(1, Ordering::Relaxed) + 1;
                                if n % opt.progress_every == 0 {
                                    if let Some(cb) = &opt.progress_callback {
                                        cb(n);
                                    }
                                }
                            }
                        }
                    }
                } else if let Ok(md) = std::fs::symlink_metadata(&child_path) {
                    if md.file_type().is_dir() {
                        if opt.max_depth == 0 || depth < opt.max_depth {
                            injector.push(Job {
                                dir: child_path,
                                depth: depth + 1,
                            });
                        }
                    } else if md.file_type().is_file() {
                        let logical = md.len();
                        if logical >= opt.min_file_size {
                            let e = map.entry(dir.to_path_buf()).or_default();
                            e.logical += logical;
                            e.physical += logical;
                            e.files += 1;
                            if opt.progress_every > 0 {
                                let n = total_files.fetch_add(1, Ordering::Relaxed) + 1;
                                if n % opt.progress_every == 0 {
                                    if let Some(cb) = &opt.progress_callback {
                                        cb(n);
                                    }
                                }
                            }
                        }
                    }
                }
            }

            bpos += d_reclen;
        }
    }
    unsafe { libc::close(fd) };
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeuristicsMode {
    Auto,
    OuterOnly,
    InnerOnly,
}
