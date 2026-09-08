//! Scanning, and the index the UI reads.
//!
//! The window used to sit blank for the whole scan and then spend seconds more
//! materialising a `Node` per entry -- 1.48M of them for a large tree, when the
//! user can only ever see a few dozen rows.
//!
//! So this does two things differently. It scans the root's children one at a
//! time and streams each subtree back as it lands, which puts rows on screen in
//! milliseconds instead of at the end. And it stores the result as a flat entry
//! vector plus a parent -> child-indices map, so the UI derives the rows it is
//! actually drawing and nothing else.
//!
//! Measured on C:/Users/syska/.cargo/registry, minimum of three warm runs:
//!
//! | shape                                | total  | first row |
//! |--------------------------------------|--------|-----------|
//! | one scan of the whole root           | 230 ms | 230 ms    |
//! | per-child, sequential, all threads   | 234 ms |   3 ms    |
//! | per-child, 4 at once, threads/4 each | 335 ms |   3 ms    |
//! | per-child, 8 at once, threads/8 each | 550 ms |   2 ms    |
//!
//! Hence one child at a time with the full thread budget: splitting threads
//! across concurrent scans starves the scanner's own work stealing and costs
//! more than it saves. Streaming is 2% slower in total and ~75x faster to first
//! paint.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc, Arc,
    },
};

use hyperdu_core::{self as core, Stat, StatMap};

/// What the worker sends back as it goes.
pub enum Msg {
    /// The root's direct children, before any size is known. One `read_dir`, so
    /// this arrives essentially immediately and gives the user something to look
    /// at while the real work runs.
    Listing {
        dirs: Vec<PathBuf>,
    },
    /// One child subtree finished. `map` covers that subtree only.
    Child {
        path: PathBuf,
        map: StatMap,
    },
    /// Files sitting directly in the root, which no child scan covers.
    RootFiles(Vec<(PathBuf, Stat)>),
    Done,
}

/// A running scan. Dropping this asks the worker to stop at the next child.
pub struct Handle {
    pub rx: mpsc::Receiver<Msg>,
    pub files_seen: Arc<AtomicU64>,
    cancel: Arc<AtomicBool>,
}

impl Handle {
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// Options a scan runs with, minus everything the UI does not expose.
#[derive(Clone)]
pub struct Params {
    pub exclude: Vec<String>,
    pub min_file_size: u64,
    pub max_depth: u32,
    pub follow_links: bool,
}

impl Params {
    fn to_options(&self, files_seen: Arc<AtomicU64>) -> core::Options {
        let counter = files_seen;
        core::Options {
            exclude_contains: self.exclude.clone(),
            max_depth: self.max_depth,
            min_file_size: self.min_file_size,
            follow_links: self.follow_links,
            // Same deliberate oversubscription the CLI uses; see
            // hyperdu_core::default_threads.
            threads: core::default_threads(),
            progress_every: 8192,
            // Only the counter. The GUI used to install a copy of the CLI's
            // hill-climbing tuner here; it was measured to fire ~13 times per
            // 106k-file scan and, on Windows, to write a knob nothing reads.
            progress_callback: Some(Arc::new(move |n| {
                counter.store(n, Ordering::Relaxed);
            })),
            compute_physical: true,
            ..core::Options::default()
        }
    }
}

/// Enumerate `root`'s children, then scan each in turn, streaming results.
pub fn start(root: PathBuf, params: Params) -> Handle {
    let (tx, rx) = mpsc::channel();
    let files_seen = Arc::new(AtomicU64::new(0));
    let cancel = Arc::new(AtomicBool::new(false));

    let worker_counter = files_seen.clone();
    let worker_cancel = cancel.clone();
    std::thread::spawn(move || {
        let (dirs, files) = list_root(&root);
        if tx.send(Msg::Listing { dirs: dirs.clone() }).is_err() {
            return;
        }
        if !files.is_empty() && tx.send(Msg::RootFiles(files)).is_err() {
            return;
        }

        let opt = params.to_options(worker_counter);
        for dir in dirs {
            if worker_cancel.load(Ordering::Relaxed) {
                return;
            }
            let map = core::scan_directory(&dir, &opt).unwrap_or_default();
            if tx.send(Msg::Child { path: dir, map }).is_err() {
                return;
            }
        }
        let _ = tx.send(Msg::Done);
    });

    Handle {
        rx,
        files_seen,
        cancel,
    }
}

/// One `read_dir` of the root: directories to scan, and the files that live
/// directly in it. Files are reported with the size we already have from the
/// directory entry, so nothing is stat'd twice.
fn list_root(root: &Path) -> (Vec<PathBuf>, Vec<(PathBuf, Stat)>) {
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    let Ok(rd) = std::fs::read_dir(root) else {
        return (dirs, files);
    };
    for entry in rd.flatten() {
        let path = entry.path();
        match entry.file_type() {
            Ok(ft) if ft.is_dir() => dirs.push(path),
            Ok(_) => {
                let len = entry.metadata().map(|m| m.len()).unwrap_or(0);
                files.push((
                    path,
                    Stat {
                        logical: len,
                        physical: len,
                        files: 1,
                    },
                ));
            }
            Err(_) => {}
        }
    }
    dirs.sort_unstable();
    (dirs, files)
}

/// Flat entries plus a parent -> children index.
///
/// The alternative shapes were measured on a 1.48M-entry tree:
///
/// | shape                            | build   | children lookup |
/// |----------------------------------|---------|-----------------|
/// | materialise the whole `Node` tree | 4.1 s  | (prebuilt)      |
/// | path-sorted vec + binary search   | 14.5 s | 768 ms          |
/// | this one                          | 2.5 s  | 11 us           |
///
/// The path-sorted variant loses because finding direct children means walking
/// the whole descendant range, and sorting paths lexicographically is dear.
#[derive(Default)]
pub struct Model {
    pub root: PathBuf,
    entries: Vec<(PathBuf, Stat)>,
    /// Values index into `entries`, ordered by physical size descending.
    children: HashMap<PathBuf, Vec<u32>>,
    /// Roots of subtrees still being scanned, so the UI can say so.
    pending: Vec<PathBuf>,
}

impl Model {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            ..Self::default()
        }
    }

    pub fn is_pending(&self, path: &Path) -> bool {
        self.pending.iter().any(|p| p == path)
    }

    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    /// Record which children exist before any of them has been scanned, so the
    /// list is complete from the first frame and only the sizes fill in.
    pub fn expect(&mut self, dirs: Vec<PathBuf>) {
        self.pending = dirs;
    }

    /// Fold one finished subtree in.
    pub fn absorb(&mut self, root_of_subtree: &Path, map: StatMap) {
        self.pending.retain(|p| p != root_of_subtree);
        self.insert_all(map.into_iter());
    }

    /// Files that sit directly in the root.
    pub fn absorb_files(&mut self, files: Vec<(PathBuf, Stat)>) {
        self.insert_all(files.into_iter());
    }

    fn insert_all(&mut self, items: impl Iterator<Item = (PathBuf, Stat)>) {
        let mut touched: Vec<PathBuf> = Vec::new();
        for (path, stat) in items {
            let Some(parent) = path.parent().map(Path::to_path_buf) else {
                continue;
            };
            let idx = self.entries.len() as u32;
            self.entries.push((path, stat));
            self.children.entry(parent.clone()).or_default().push(idx);
            if !touched.contains(&parent) {
                touched.push(parent);
            }
        }
        // Re-sort only the parents this batch actually changed.
        for parent in touched {
            if let Some(v) = self.children.get_mut(&parent) {
                let entries = &self.entries;
                v.sort_unstable_by_key(|&i| std::cmp::Reverse(entries[i as usize].1.physical));
            }
        }
    }

    /// Direct children of `dir`, largest first. Empty when nothing is known yet.
    pub fn children_of(&self, dir: &Path) -> &[u32] {
        self.children.get(dir).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn entry(&self, index: u32) -> (&Path, &Stat) {
        let (p, s) = &self.entries[index as usize];
        (p.as_path(), s)
    }

    /// Whether `dir` has anything under it, used to decide if a row is
    /// expandable without materialising its children.
    pub fn has_children(&self, dir: &Path) -> bool {
        self.children.get(dir).is_some_and(|v| !v.is_empty())
    }

    /// Total for a directory. The root's own total is the sum of its children,
    /// because no single scan covered the whole root.
    pub fn total_of(&self, dir: &Path) -> Stat {
        if dir == self.root {
            return self
                .children_of(dir)
                .iter()
                .fold(Stat::default(), |mut acc, &i| {
                    let (_, s) = self.entry(i);
                    acc.logical += s.logical;
                    acc.physical += s.physical;
                    acc.files += s.files;
                    acc
                });
        }
        self.children
            .get(dir.parent().unwrap_or(dir))
            .and_then(|v| {
                v.iter()
                    .map(|&i| self.entry(i))
                    .find(|(p, _)| *p == dir)
                    .map(|(_, s)| *s)
            })
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stat(physical: u64) -> Stat {
        Stat {
            logical: physical,
            physical,
            files: 1,
        }
    }

    /// Synthetic paths only; nothing here touches a filesystem.
    fn subtree(pairs: &[(&str, u64)]) -> StatMap {
        pairs
            .iter()
            .map(|(p, size)| (PathBuf::from(p), stat(*size)))
            .collect()
    }

    #[test]
    fn children_are_ordered_by_physical_size_descending() {
        let mut m = Model::new(PathBuf::from("/root"));
        m.absorb(
            Path::new("/root/a"),
            subtree(&[("/root/a", 10), ("/root/a/small", 1), ("/root/a/big", 9)]),
        );

        let names: Vec<_> = m
            .children_of(Path::new("/root/a"))
            .iter()
            .map(|&i| m.entry(i).0.file_name().unwrap().to_str().unwrap())
            .collect();
        assert_eq!(names, ["big", "small"]);
    }

    #[test]
    fn ordering_survives_a_later_subtree_landing_in_the_same_parent() {
        // Subtrees stream in one at a time, so a parent gets appended to more
        // than once and must be re-sorted each time, not just on first insert.
        let mut m = Model::new(PathBuf::from("/root"));
        m.absorb(Path::new("/root/a"), subtree(&[("/root/a", 5)]));
        m.absorb(Path::new("/root/b"), subtree(&[("/root/b", 50)]));
        m.absorb(Path::new("/root/c"), subtree(&[("/root/c", 20)]));

        let names: Vec<_> = m
            .children_of(Path::new("/root"))
            .iter()
            .map(|&i| m.entry(i).0.file_name().unwrap().to_str().unwrap())
            .collect();
        assert_eq!(names, ["b", "c", "a"]);
    }

    #[test]
    fn root_total_is_the_sum_of_its_children() {
        // No single scan covers the root, so its total has to be derived.
        let mut m = Model::new(PathBuf::from("/root"));
        m.absorb(Path::new("/root/a"), subtree(&[("/root/a", 30)]));
        m.absorb(Path::new("/root/b"), subtree(&[("/root/b", 12)]));

        let total = m.total_of(Path::new("/root"));
        assert_eq!(total.physical, 42);
        assert_eq!(total.files, 2);
    }

    #[test]
    fn total_of_a_child_reads_the_entry_recorded_for_it() {
        let mut m = Model::new(PathBuf::from("/root"));
        m.absorb(Path::new("/root/a"), subtree(&[("/root/a", 7)]));
        assert_eq!(m.total_of(Path::new("/root/a")).physical, 7);
    }

    #[test]
    fn unknown_directory_has_no_children_and_no_total() {
        let m = Model::new(PathBuf::from("/root"));
        assert!(m.children_of(Path::new("/root/nope")).is_empty());
        assert!(!m.has_children(Path::new("/root/nope")));
        assert_eq!(m.total_of(Path::new("/root/nope")).physical, 0);
    }

    #[test]
    fn a_leaf_reports_no_children_so_the_tree_does_not_offer_to_expand_it() {
        let mut m = Model::new(PathBuf::from("/root"));
        m.absorb(
            Path::new("/root/a"),
            subtree(&[("/root/a", 3), ("/root/a/file", 3)]),
        );
        assert!(m.has_children(Path::new("/root/a")));
        assert!(!m.has_children(Path::new("/root/a/file")));
    }

    #[test]
    fn absorbing_a_subtree_clears_it_from_pending() {
        let mut m = Model::new(PathBuf::from("/root"));
        m.expect(vec![PathBuf::from("/root/a"), PathBuf::from("/root/b")]);
        assert_eq!(m.pending_count(), 2);
        assert!(m.is_pending(Path::new("/root/a")));

        m.absorb(Path::new("/root/a"), subtree(&[("/root/a", 1)]));
        assert_eq!(m.pending_count(), 1);
        assert!(!m.is_pending(Path::new("/root/a")));
        assert!(m.is_pending(Path::new("/root/b")));
    }

    #[test]
    fn root_files_are_listed_beside_the_directories() {
        let mut m = Model::new(PathBuf::from("/root"));
        m.absorb(Path::new("/root/a"), subtree(&[("/root/a", 5)]));
        m.absorb_files(vec![(PathBuf::from("/root/loose.txt"), stat(100))]);

        let names: Vec<_> = m
            .children_of(Path::new("/root"))
            .iter()
            .map(|&i| m.entry(i).0.file_name().unwrap().to_str().unwrap())
            .collect();
        assert_eq!(names, ["loose.txt", "a"]);
        assert_eq!(m.entry_count(), 2);
    }
}
