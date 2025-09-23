use std::sync::Arc;

use crate::{platform, DirContext, ScanContext, StatMap};

/// Abstraction over filesystem enumeration to improve testability.
/// The default implementation delegates to platform backends.
pub trait FileSystemScanner: Send + Sync {
    fn process_dir(&self, ctx: &ScanContext, dctx: &DirContext, map: &mut StatMap);
}

/// Default scanner that calls into OS-specific backends.
#[derive(Default, Clone, Copy)]
pub struct PlatformScanner;

impl FileSystemScanner for PlatformScanner {
    #[inline]
    fn process_dir(&self, ctx: &ScanContext, dctx: &DirContext, map: &mut StatMap) {
        platform::process_dir_wrapped(ctx, dctx, map)
    }
}

#[inline]
pub fn platform_scanner() -> PlatformScanner {
    PlatformScanner
}

/// Experimental: scan multiple roots in parallel using rayon.
/// This runs independent `scan_directory_with` invocations and merges their maps.
/// Note: each scan may also spawn threads internally based on `Options.threads`.
/// Consider lowering `Options.threads` to avoid oversubscription.
#[cfg(feature = "rayon-par")]
pub fn parallel_scan(
    roots: Vec<std::path::PathBuf>,
    opt: &crate::Options,
) -> anyhow::Result<crate::StatMap> {
    use rayon::prelude::*;

    // Balance threads: distribute roughly evenly across rayon workers
    let mut opt_local = opt.clone();
    let n_rayon = rayon::current_num_threads().max(1);
    let n_roots = roots.len().max(1);
    if opt_local.threads > 1 {
        let denom = std::cmp::max(1, std::cmp::min(n_rayon, n_roots));
        let per = std::cmp::max(1, opt_local.threads / denom);
        opt_local.threads = per;
    }
    let scanner = Arc::new(platform_scanner());
    roots
        .into_par_iter()
        .map(|r| {
            if opt_local.prefer_inner_rayon {
                #[cfg(feature = "rayon-inner")]
                {
                    return crate::scan_directory_rayon(r, &opt_local);
                }
            }
            crate::scan_directory_with(r, &opt_local, scanner.clone())
        })
        .try_reduce(
            || ahash::AHashMap::default(),
            |mut acc, map| {
                for (k, v) in map {
                    let e = acc.entry(k).or_default();
                    e.logical += v.logical;
                    e.physical += v.physical;
                    e.files += v.files;
                }
                Ok(acc)
            },
        )
}

/// Dynamic heuristic: choose between parallel_scan and sequential scan based on
/// number of roots and available CPUs. Distributes threads to avoid oversubscription.
#[cfg(feature = "rayon-par")]
pub fn auto_parallel_scan(
    roots: Vec<std::path::PathBuf>,
    opt: &crate::Options,
) -> anyhow::Result<crate::StatMap> {
    if roots.is_empty() {
        return Ok(ahash::AHashMap::default());
    }
    let cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    let n_roots = roots.len();
    use crate::HeuristicsMode::*;
    if matches!(opt.heuristics_mode, OuterOnly) {
        return parallel_scan(roots, opt);
    }
    if matches!(opt.heuristics_mode, InnerOnly) || n_roots == 1 {
        // Single root: rely on inner parallelism (manual threads or rayon-inner if enabled)
        #[cfg(feature = "rayon-inner")]
        {
            return crate::scan_directory_rayon(&roots[0], opt);
        }
        #[cfg(not(feature = "rayon-inner"))]
        {
            return crate::scan_directory(&roots[0], opt);
        }
    }
    // Initial probe: estimate top-level width for a few roots (fast read_dir)
    let mut total_width: usize = 0;
    let sample_n = std::cmp::min(4, n_roots);
    for r in roots.iter().take(sample_n) {
        total_width += estimate_root_width(r, 200);
    }
    let avg_width = if sample_n > 0 {
        total_width / sample_n
    } else {
        0
    };
    // Auto: prefer outer parallelism when many roots or small per-root threads, unless width is very large
    if n_roots >= cpus / 2 || opt.threads <= 2 {
        if avg_width < 4096 {
            return parallel_scan(roots, opt);
        }
    }
    // If width is high, prefer inner parallelism
    if avg_width >= 4096 {
        let mut acc: ahash::AHashMap<std::path::PathBuf, crate::Stat> = ahash::AHashMap::default();
        for r in roots {
            #[cfg(feature = "rayon-inner")]
            let map = crate::scan_directory_rayon(&r, opt)?;
            #[cfg(not(feature = "rayon-inner"))]
            let map = crate::scan_directory(&r, opt)?;
            for (k, v) in map {
                let e = acc.entry(k).or_default();
                e.logical += v.logical;
                e.physical += v.physical;
                e.files += v.files;
            }
        }
        return Ok(acc);
    }
    // Else fall back to sequential outer scans using full inner threads
    // Otherwise run sequential outer, full inner threads
    let mut acc: ahash::AHashMap<std::path::PathBuf, crate::Stat> = ahash::AHashMap::default();
    for r in roots {
        #[cfg(feature = "rayon-inner")]
        let map = crate::scan_directory_rayon(&r, opt)?;
        #[cfg(not(feature = "rayon-inner"))]
        let map = crate::scan_directory(&r, opt)?;
        for (k, v) in map {
            let e = acc.entry(k).or_default();
            e.logical += v.logical;
            e.physical += v.physical;
            e.files += v.files;
        }
    }
    Ok(acc)
}

#[cfg(feature = "rayon-par")]
fn estimate_root_width(root: &std::path::Path, budget_ms: u64) -> usize {
    use std::time::{Duration, Instant};
    let t0 = Instant::now();
    let mut n = 0usize;
    if let Ok(mut rd) = std::fs::read_dir(root) {
        while let Some(Ok(_)) = rd.next() {
            n += 1;
            if t0.elapsed() >= Duration::from_millis(budget_ms) || n >= 8192 {
                break;
            }
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashSet,
        path::{Path, PathBuf},
    };

    use ahash::AHashMap as HashMap;

    use super::*;
    use crate::{Options, Stat};

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum MockKind {
        Dir,
        File(u64),
        SymlinkDir(PathBuf),
        SymlinkFile(u64),
    }

    struct MockFileSystem {
        entries: HashMap<PathBuf, Vec<(String, MockKind)>>,
        visited: std::sync::Mutex<HashSet<PathBuf>>,
    }

    impl MockFileSystem {
        fn with_dir(mut self, dir: &Path, items: Vec<(String, MockKind)>) -> Self {
            self.entries.insert(dir.to_path_buf(), items);
            self
        }
    }

    impl Default for MockFileSystem {
        fn default() -> Self {
            Self {
                entries: HashMap::default(),
                visited: std::sync::Mutex::new(HashSet::new()),
            }
        }
    }

    impl FileSystemScanner for MockFileSystem {
        fn process_dir(&self, ctx: &ScanContext, dctx: &DirContext, map: &mut StatMap) {
            let opt = ctx.options;
            let dir = dctx.dir;
            let depth = dctx.depth;
            let stat_cur = map.entry(dir.to_path_buf()).or_insert(Stat::default());
            let Some(items) = self.entries.get(dir) else {
                return;
            };
            for (name, kind) in items {
                // Build child path
                let child = dir.join(name);
                if crate::path_excluded(&child, opt) {
                    continue;
                }
                match kind {
                    MockKind::Dir => {
                        if opt.max_depth == 0 || depth < opt.max_depth {
                            // simple visited set to approximate loop detection
                            let mut v = self.visited.lock().unwrap();
                            if v.insert(child.clone()) {
                                ctx.enqueue_dir(child, depth + 1);
                            }
                        }
                    }
                    MockKind::File(sz) => {
                        if *sz >= opt.min_file_size {
                            stat_cur.logical += *sz;
                            stat_cur.physical += if opt.compute_physical { *sz } else { *sz };
                            stat_cur.files += 1;
                            ctx.report_progress(opt, Some(&child));
                        }
                    }
                    MockKind::SymlinkDir(target) => {
                        if !opt.follow_links {
                            continue;
                        }
                        if opt.max_depth == 0 || depth < opt.max_depth {
                            let mut v = self.visited.lock().unwrap();
                            if v.insert(target.clone()) {
                                ctx.enqueue_dir(target.clone(), depth + 1);
                            }
                        }
                    }
                    MockKind::SymlinkFile(sz) => {
                        if !opt.follow_links {
                            continue;
                        }
                        if *sz >= opt.min_file_size {
                            stat_cur.logical += *sz;
                            stat_cur.physical += if opt.compute_physical { *sz } else { *sz };
                            stat_cur.files += 1;
                            ctx.report_progress(opt, Some(&child));
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn mock_scanner_basic_rollup() {
        use std::sync::{atomic::AtomicUsize, Arc};

        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("r");
        std::fs::create_dir_all(&root).unwrap();

        // Layout: r/{a:10, d/{b:20}}
        let mock = MockFileSystem::default()
            .with_dir(
                &root,
                vec![
                    ("a".into(), MockKind::File(10)),
                    ("d".into(), MockKind::Dir),
                ],
            )
            .with_dir(&root.join("d"), vec![("b".into(), MockKind::File(20))]);

        let mut opt = Options::default();
        opt.compute_physical = false; // simplify
                                      // ensure progress system present but quiet
        opt.progress_every = 0;
        opt.dir_yield_every = Arc::new(AtomicUsize::new(0));

        let map = crate::scan_directory_with(&root, &opt, std::sync::Arc::new(mock)).unwrap();
        let s_root = map.get(&root).copied().unwrap_or_default();
        assert_eq!(s_root.files, 2);
        assert_eq!(s_root.logical, 30);
        assert_eq!(s_root.physical, 30);
    }

    #[test]
    fn filter_exclude_contains() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("r");
        std::fs::create_dir_all(&root).unwrap();
        let mock = MockFileSystem::default().with_dir(
            &root,
            vec![
                ("skip_a".into(), MockKind::File(10)),
                ("keep_b".into(), MockKind::File(20)),
            ],
        );
        let mut opt = Options::default();
        opt.exclude_contains = vec!["skip".into()];
        let map = crate::scan_directory_with(&root, &opt, std::sync::Arc::new(mock)).unwrap();
        let s_root = map.get(&root).copied().unwrap_or_default();
        assert_eq!(s_root.files, 1);
        assert_eq!(s_root.logical, 20);
    }

    #[test]
    fn filter_glob_excludes_subdir() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("r");
        let d = root.join("d");
        std::fs::create_dir_all(&d).unwrap();
        let mock = MockFileSystem::default()
            .with_dir(
                &root,
                vec![
                    ("a".into(), MockKind::File(10)),
                    ("d".into(), MockKind::Dir),
                ],
            )
            .with_dir(&d, vec![("b".into(), MockKind::File(20))]);
        let mut opt = Options::default();
        opt.exclude_glob = vec!["**/d/**".into()];
        let map = crate::scan_directory_with(&root, &opt, std::sync::Arc::new(mock)).unwrap();
        let s_root = map.get(&root).copied().unwrap_or_default();
        assert_eq!(s_root.files, 1);
        assert_eq!(s_root.logical, 10);
    }

    #[test]
    fn filter_regex_excludes_tmp() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("r");
        std::fs::create_dir_all(&root).unwrap();
        let mock = MockFileSystem::default().with_dir(
            &root,
            vec![
                ("x.tmp".into(), MockKind::File(5)),
                ("x.log".into(), MockKind::File(7)),
            ],
        );
        let mut opt = Options::default();
        opt.exclude_regex = vec![".*\\.tmp$".into()];
        let map = crate::scan_directory_with(&root, &opt, std::sync::Arc::new(mock)).unwrap();
        let s_root = map.get(&root).copied().unwrap_or_default();
        assert_eq!(s_root.files, 1);
        assert_eq!(s_root.logical, 7);
    }

    #[test]
    fn max_depth_limits_grandchildren() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("r");
        let d = root.join("d");
        let e = d.join("e");
        std::fs::create_dir_all(&e).unwrap();
        let mock = MockFileSystem::default()
            .with_dir(
                &root,
                vec![("a".into(), MockKind::File(1)), ("d".into(), MockKind::Dir)],
            )
            .with_dir(
                &d,
                vec![("b".into(), MockKind::File(2)), ("e".into(), MockKind::Dir)],
            )
            .with_dir(&e, vec![("c".into(), MockKind::File(4))]);
        let mut opt = Options::default();
        opt.max_depth = 1; // allow scanning root (0) and its children (1), not grandchildren
        let map = crate::scan_directory_with(&root, &opt, std::sync::Arc::new(mock)).unwrap();
        let s_root = map.get(&root).copied().unwrap_or_default();
        assert_eq!(s_root.files, 2); // a + b
        assert_eq!(s_root.logical, 3);
    }

    #[test]
    fn min_file_size_filters_small_files() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("r");
        std::fs::create_dir_all(&root).unwrap();
        let mock = MockFileSystem::default().with_dir(
            &root,
            vec![
                ("small".into(), MockKind::File(10)),
                ("big".into(), MockKind::File(100)),
            ],
        );
        let mut opt = Options::default();
        opt.min_file_size = 50;
        let map = crate::scan_directory_with(&root, &opt, std::sync::Arc::new(mock)).unwrap();
        let s_root = map.get(&root).copied().unwrap_or_default();
        assert_eq!(s_root.files, 1);
        assert_eq!(s_root.logical, 100);
    }

    #[test]
    fn symlink_follow_false_skips() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("r");
        let d = root.join("d");
        std::fs::create_dir_all(&d).unwrap();
        let mock = MockFileSystem::default()
            .with_dir(&root, vec![("d".into(), MockKind::SymlinkDir(d.clone()))])
            .with_dir(&d, vec![("in_d".into(), MockKind::File(5))]);
        let mut opt = Options::default();
        opt.follow_links = false;
        let map = crate::scan_directory_with(&root, &opt, std::sync::Arc::new(mock)).unwrap();
        let s_root = map.get(&root).copied().unwrap_or_default();
        assert_eq!(s_root.files, 0);
        assert_eq!(s_root.logical, 0);
    }

    #[test]
    fn symlink_follow_true_counts_and_guards_cycles() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("r");
        let d = root.join("d");
        std::fs::create_dir_all(&d).unwrap();
        let mock = MockFileSystem::default()
            .with_dir(
                &root,
                vec![("d_link".into(), MockKind::SymlinkDir(d.clone()))],
            )
            .with_dir(
                &d,
                vec![
                    ("in_d".into(), MockKind::File(5)),
                    // cycle: symlink from d back to root
                    ("back".into(), MockKind::SymlinkDir(root.clone())),
                ],
            );
        let mut opt = Options::default();
        opt.follow_links = true;
        let map = crate::scan_directory_with(&root, &opt, std::sync::Arc::new(mock)).unwrap();
        let s_root = map.get(&root).copied().unwrap_or_default();
        assert_eq!(s_root.files, 1);
        assert_eq!(s_root.logical, 5);
    }
}
