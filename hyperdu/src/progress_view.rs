//! Scan progress on stderr.
//!
//! On a terminal the status is two lines redrawn in place -- the count with a
//! percentage, and one sample file -- and they are erased when the scan ends so
//! only the result remains. Redirected, it falls back to one status pair per
//! interval, since cursor movement would only litter a log.
//!
//! The percentage needs a total nobody knows up front, so it is estimated from
//! whatever basis the scan offers; see [`Shared::fraction`]. Counting the tree
//! in parallel was tried and dropped: on Windows it costs about as much as the
//! scan itself (27s -> 40s on 1.6M files) and still finished last.
use std::{
    collections::HashMap,
    io::Write,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc, Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

use humansize::{format_size, BINARY};

// 50 redraws a second: at 50ms the steps were still visible. A redraw is two
// short writes, so the scan does not notice.
const LIVE_INTERVAL: Duration = Duration::from_millis(20);
// ponytail: assumes a terminal at least 80 columns wide. A narrower one wraps
// the lines and the in-place redraw drifts; query the width if that matters.
const MAX_LINE: usize = 78;

pub(crate) struct Shared {
    /// Files finished by roots that are already done.
    base: AtomicU64,
    /// Files reported by the scan of the current root.
    files: AtomicU64,
    /// Filled in by the scan for the percentage.
    counters: Arc<hyperdu_core::ProgressCounters>,
    /// File count of the previous scan of the same roots, if one was recorded.
    recorded: Option<u64>,
    /// Used bytes of the volume, when the scan root is the volume's root.
    volume_used: Option<u64>,
    shown_pct: AtomicU64,
    sample: Mutex<String>,
    /// Held while drawing, and while anything else writes to the terminal.
    screen: Mutex<bool>,
    live: bool,
    started: Instant,
    /// Identifies the roots in the total cache; `None` if one cannot be resolved.
    cache_key: Option<String>,
}

impl Shared {
    pub(crate) fn update(&self, files: u64) {
        self.files.fetch_max(files, Ordering::Relaxed);
    }

    pub(crate) fn sample(&self, path: &Path, size: u64) {
        let line = format!("scan: {} ({})", short_path(path), format_size(size, BINARY));
        *self.sample.lock().unwrap_or_else(|e| e.into_inner()) = line;
    }

    /// A root finished: the next scan's counter starts again from zero.
    pub(crate) fn root_done(&self, files: u64) {
        self.base.fetch_add(files, Ordering::Relaxed);
        self.files.store(0, Ordering::Relaxed);
    }

    /// Remember the scan's exact file count for the next run of the same roots.
    pub(crate) fn record_total(&self, files: u64) {
        let (Some(key), Some(path)) = (&self.cache_key, cache_path()) else {
            return;
        };
        // Best effort: a lost record only costs the next run its percentage.
        let mut totals = load_totals(&path);
        totals.insert(key.clone(), files);
        save_totals(&path, &totals);
    }

    /// Erase the status, run `f` (which may print), and let the next tick
    /// redraw below whatever `f` wrote.
    pub(crate) fn hold<T>(&self, f: impl FnOnce() -> T) -> T {
        let mut drawn = self.screen.lock().unwrap_or_else(|e| e.into_inner());
        if std::mem::take(&mut *drawn) {
            eprint!("\r\x1b[2K\x1b[1A\r\x1b[2K");
        }
        f()
    }

    /// `(done, total)` from the best basis this scan has, in order: `$MFT`
    /// records (admin `--mft`), bytes against the volume's used space (a drive
    /// root), the previous run's file count, and depth-1/2 directories finished.
    fn fraction(&self, files: u64) -> Option<(u64, u64)> {
        let c = &self.counters;
        let load = |a: &AtomicU64| a.load(Ordering::Relaxed);
        let mft_total = load(&c.mft_records_total);
        if mft_total > 0 {
            return Some((load(&c.mft_records), mft_total));
        }
        if let Some(used) = self.volume_used {
            return Some((load(&c.bytes), used));
        }
        if let Some(t) = self.recorded {
            return Some((files, t));
        }
        let dirs = load(&c.dirs_total);
        (dirs > 0).then(|| (load(&c.dirs_done), dirs))
    }

    fn status(&self, finished: bool) -> String {
        let done = self.base.load(Ordering::Relaxed) + self.files.load(Ordering::Relaxed);
        let secs = self.started.elapsed().as_secs_f64();
        let rate = done as f64 / secs.max(1e-6);
        let count = match (finished, self.recorded) {
            (false, Some(t)) if self.volume_used.is_none() => format!("{done} / {t} files"),
            _ => format!("{done} files"),
        };
        let pct = if finished {
            "100%".to_string()
        } else if let Some((part, whole)) = self.fraction(done) {
            // Every basis is an estimate of the scan, so none may claim
            // completion before the scan does. The directory basis still
            // discovers its total as it goes; a percentage that falls back
            // reads as a fault, so the shown value only rises.
            let pct = (part.saturating_mul(100) / whole.max(1)).min(99);
            let pct = self.shown_pct.fetch_max(pct, Ordering::Relaxed).max(pct);
            format!("{pct:>3}%")
        } else {
            " --%".to_string()
        };
        format!("progress: {pct} | {count} | {rate:.0} f/s | {secs:.1}s")
    }

    fn draw(&self, finished: bool) {
        let status = self.status(finished);
        let sample = self
            .sample
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let mut drawn = self.screen.lock().unwrap_or_else(|e| e.into_inner());
        let mut err = std::io::stderr().lock();
        if self.live {
            let up = if *drawn { "\r\x1b[1A" } else { "" };
            let _ = write!(
                err,
                "{up}\r{}\x1b[K\n{}\x1b[K",
                truncate(&status),
                truncate(&sample)
            );
            *drawn = true;
        } else {
            let _ = writeln!(err, "{status}");
            if !sample.is_empty() {
                let _ = writeln!(err, "{sample}");
            }
        }
        let _ = err.flush();
    }
}

pub(crate) struct ProgressView {
    shared: Arc<Shared>,
    stop_render: Option<mpsc::SyncSender<()>>,
    renderer: Option<thread::JoinHandle<()>>,
}

impl ProgressView {
    /// `live` redraws in place; it is ignored when the terminal cannot take
    /// the escape sequences.
    pub(crate) fn start(roots: &[PathBuf], live: bool) -> Self {
        let live = live && enable_vt();
        let cache_key = roots
            .iter()
            .map(|r| {
                std::fs::canonicalize(r)
                    .ok()
                    .map(|c| c.to_string_lossy().into_owned())
            })
            .collect::<Option<Vec<_>>>()
            .map(|v| v.join("\n"));
        let recorded = cache_key
            .as_ref()
            .zip(cache_path())
            .and_then(|(k, p)| load_totals(&p).get(k).copied());
        let volume_used = match roots {
            [root] if is_volume_root(root) => hyperdu_core::volume::total_free(root)
                .map(|(total, free)| total.saturating_sub(free))
                .filter(|&used| used > 0),
            _ => None,
        };
        let shared = Arc::new(Shared {
            base: AtomicU64::new(0),
            files: AtomicU64::new(0),
            counters: Arc::default(),
            recorded,
            volume_used,
            shown_pct: AtomicU64::new(0),
            sample: Mutex::new(String::new()),
            screen: Mutex::new(false),
            live,
            started: Instant::now(),
            cache_key,
        });
        if !live {
            eprintln!("scanning …");
        }
        let interval = if live {
            LIVE_INTERVAL
        } else {
            let secs = std::env::var("HYPERDU_PROGRESS_KEEPALIVE_SECS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(1)
                .max(1);
            Duration::from_secs(secs)
        };
        let (stop_render, stopped) = mpsc::sync_channel(1);
        let renderer = {
            let shared = shared.clone();
            thread::spawn(move || {
                // Completion wakes the wait immediately, including a very short scan.
                while let Err(mpsc::RecvTimeoutError::Timeout) = stopped.recv_timeout(interval) {
                    shared.draw(false);
                }
            })
        };
        Self {
            shared,
            stop_render: Some(stop_render),
            renderer: Some(renderer),
        }
    }

    pub(crate) fn shared(&self) -> Arc<Shared> {
        self.shared.clone()
    }

    /// To install as `Options::progress_counters`.
    pub(crate) fn counters(&self) -> Arc<hyperdu_core::ProgressCounters> {
        self.shared.counters.clone()
    }
}

impl Drop for ProgressView {
    fn drop(&mut self) {
        if let Some(stop) = self.stop_render.take() {
            let _ = stop.send(());
        }
        if let Some(t) = self.renderer.take() {
            let _ = t.join();
        }
        if self.shared.live {
            self.shared.hold(|| ());
        } else {
            self.shared.draw(true);
        }
    }
}

fn cache_path() -> Option<PathBuf> {
    #[cfg(windows)]
    let dir = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    #[cfg(not(windows))]
    let dir = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")));
    dir.map(|d| d.join("hyperdu").join("progress-totals.json"))
}

fn load_totals(path: &Path) -> HashMap<String, u64> {
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save_totals(path: &Path, totals: &HashMap<String, u64>) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_vec(totals) {
        let _ = std::fs::write(path, json);
    }
}

/// A drive root on Windows, or a mount point on Unix: where the volume's used
/// space is the size of the scan.
fn is_volume_root(root: &Path) -> bool {
    let Ok(path) = std::fs::canonicalize(root) else {
        return false;
    };
    let Some(parent) = path.parent() else {
        return true;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        matches!(
            (std::fs::metadata(&path), std::fs::metadata(parent)),
            (Ok(a), Ok(b)) if a.dev() != b.dev()
        )
    }
    #[cfg(not(unix))]
    {
        let _ = parent;
        false
    }
}

fn short_path(p: &Path) -> String {
    if let Some(n) = p.file_name().and_then(|s| s.to_str()) {
        return n.to_string();
    }
    p.to_string_lossy().into_owned()
}

fn truncate(s: &str) -> String {
    if s.chars().count() <= MAX_LINE {
        return s.to_string();
    }
    let mut t: String = s.chars().take(MAX_LINE - 1).collect();
    t.push('…');
    t
}

/// Windows consoles interpret escape sequences only once asked to.
#[cfg(windows)]
fn enable_vt() -> bool {
    use windows::Win32::System::Console::{
        GetConsoleMode, GetStdHandle, SetConsoleMode, CONSOLE_MODE,
        ENABLE_VIRTUAL_TERMINAL_PROCESSING, STD_ERROR_HANDLE,
    };
    // SAFETY: plain console API calls on the process's own stderr handle.
    unsafe {
        let Ok(h) = GetStdHandle(STD_ERROR_HANDLE) else {
            return false;
        };
        let mut mode = CONSOLE_MODE::default();
        GetConsoleMode(h, &mut mode).is_ok()
            && SetConsoleMode(h, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING).is_ok()
    }
}

#[cfg(not(windows))]
fn enable_vt() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recorded_totals_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("totals.json");
        assert!(load_totals(&path).is_empty());
        save_totals(&path, &HashMap::from([("root".to_string(), 42)]));
        assert_eq!(load_totals(&path).get("root"), Some(&42));
    }

    fn shared(recorded: Option<u64>, volume_used: Option<u64>) -> Shared {
        Shared {
            base: AtomicU64::new(0),
            files: AtomicU64::new(0),
            counters: Arc::default(),
            recorded,
            volume_used,
            shown_pct: AtomicU64::new(0),
            sample: Mutex::new(String::new()),
            screen: Mutex::new(false),
            live: false,
            started: Instant::now(),
            cache_key: None,
        }
    }

    #[test]
    fn percentage_stays_below_100_until_finished() {
        let s = shared(Some(100), None);
        s.update(120);
        assert!(s.status(false).contains(" 99%"));
        assert!(s.status(true).contains("100%"));
    }

    #[test]
    fn basis_prefers_mft_then_volume_then_record_then_directories() {
        let s = shared(Some(1000), Some(400));
        s.update(500);
        s.counters.bytes.store(100, Ordering::Relaxed);
        s.counters.dirs_total.store(10, Ordering::Relaxed);
        s.counters.dirs_done.store(9, Ordering::Relaxed);
        assert_eq!(s.fraction(500), Some((100, 400)));
        s.counters.mft_records_total.store(8, Ordering::Relaxed);
        s.counters.mft_records.store(2, Ordering::Relaxed);
        assert_eq!(s.fraction(500), Some((2, 8)));
        assert_eq!(shared(Some(1000), None).fraction(500), Some((500, 1000)));
        let d = shared(None, None);
        assert_eq!(d.fraction(0), None);
        d.counters.dirs_total.store(4, Ordering::Relaxed);
        d.counters.dirs_done.store(1, Ordering::Relaxed);
        assert_eq!(d.fraction(0), Some((1, 4)));
    }

    #[test]
    fn shown_percentage_never_falls() {
        let s = shared(None, None);
        s.counters.dirs_total.store(4, Ordering::Relaxed);
        s.counters.dirs_done.store(2, Ordering::Relaxed);
        assert!(s.status(false).contains(" 50%"));
        // More directories turn up; the estimate drops but the display holds.
        s.counters.dirs_total.store(10, Ordering::Relaxed);
        assert!(s.status(false).contains(" 50%"));
    }
}
