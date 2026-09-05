//! Directory-level aggregates for the persistent index (#16), stage 2.
//!
//! The point of an index is to answer "how big is this tree" *without walking
//! it*. The previous attempt (`incremental.rs`) stored one record per file and
//! still walked the tree to detect changes, which measured 11x slower than
//! simply scanning -- walking in order to avoid walking. See
//! `docs/design/persistent-index.md`.
//!
//! So this stores aggregates per directory, keyed by inode:
//!
//!   * `subtree_bytes` means reading the root's single entry answers the whole
//!     question. That is what "not walking" actually means here.
//!   * The key is `(dev, ino)` rather than a path string, so a rename does not
//!     invalidate the entry and paths are not duplicated across every key.
//!   * Freshness is carried explicitly, because the worst outcome is a caller
//!     believing a stale total.
//!
//! This stage has no persistence and no inotify: the aggregates and their
//! propagation are settled first. Whether a resident process is acceptable is a
//! product decision the design document deliberately leaves open.

use ahash::AHashMap as HashMap;

/// Identity of a directory, stable across renames.
pub type DirKey = (u64, u64);

/// How much a caller may trust an entry.
///
/// Carried per entry rather than per index: a watch limit or a queue overflow
/// invalidates one subtree, not the whole tree, and dropping the rest would
/// throw away work that is still good.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Freshness {
    /// Watched continuously since the last scan; no events were missed.
    Fresh,
    /// Scanned once, but events may have been missed since -- the watcher was
    /// not running, hit its limit, or overflowed its queue.
    Stale,
    /// Never scanned. Distinct from `Stale`: there is no number to show.
    Unknown,
}

/// Aggregates for one directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirEntry {
    /// Files directly in this directory, not counting subdirectories.
    pub own_bytes: u64,
    pub own_files: u64,
    /// This directory plus everything under it. Reading the root's value
    /// answers the whole question.
    pub subtree_bytes: u64,
    pub subtree_files: u64,
    /// Bumped on every update, so a propagation pass can tell "already applied"
    /// from "not yet" and a diff is not added to an ancestor twice.
    pub generation: u64,
    pub state: Freshness,
}

impl Default for DirEntry {
    fn default() -> Self {
        Self {
            own_bytes: 0,
            own_files: 0,
            subtree_bytes: 0,
            subtree_files: 0,
            generation: 0,
            state: Freshness::Unknown,
        }
    }
}

/// Directory aggregates plus the parent links needed to propagate a change.
#[derive(Debug, Default)]
pub struct Index {
    entries: HashMap<DirKey, DirEntry>,
    /// Child -> parent. Held separately from `DirEntry` so an entry can be
    /// updated without touching the tree shape.
    parents: HashMap<DirKey, DirKey>,
    generation: u64,
}

impl Index {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn get(&self, key: DirKey) -> Option<&DirEntry> {
        self.entries.get(&key)
    }

    pub fn parent_of(&self, key: DirKey) -> Option<DirKey> {
        self.parents.get(&key).copied()
    }

    /// Record a directory's own contents, and propagate the change upwards.
    ///
    /// Propagation applies the *difference* rather than recomputing, so an
    /// update costs the depth of the directory instead of the size of the tree.
    /// That is the whole point: without it, one changed file would mean
    /// re-summing everything above it.
    pub fn set_own(&mut self, key: DirKey, own_bytes: u64, own_files: u64) {
        self.generation += 1;
        let gen = self.generation;

        let entry = self.entries.entry(key).or_default();
        let d_bytes = own_bytes as i128 - entry.own_bytes as i128;
        let d_files = own_files as i128 - entry.own_files as i128;

        entry.own_bytes = own_bytes;
        entry.own_files = own_files;
        entry.subtree_bytes = apply_delta(entry.subtree_bytes, d_bytes);
        entry.subtree_files = apply_delta(entry.subtree_files, d_files);
        entry.generation = gen;
        // A directory whose contents were just measured is as fresh as it gets.
        entry.state = Freshness::Fresh;

        self.propagate(key, d_bytes, d_files, gen);
    }

    /// Attach `child` under `parent`, moving the child's subtree totals with it.
    ///
    /// Used both when a directory is first seen and when it is renamed into a
    /// different parent. A rename must not lose the subtree's bytes, and must
    /// not leave them counted under the old parent.
    pub fn link(&mut self, child: DirKey, parent: DirKey) {
        if self.parents.get(&child) == Some(&parent) {
            return;
        }
        let (bytes, files) = match self.entries.get(&child) {
            Some(e) => (e.subtree_bytes as i128, e.subtree_files as i128),
            None => (0, 0),
        };

        self.generation += 1;
        let gen = self.generation;

        // Take the subtree away from the old parent before giving it to the new
        // one, or the bytes exist in two places at once.
        if let Some(old) = self.parents.get(&child).copied() {
            self.propagate_from(old, -bytes, -files, gen);
        }
        self.parents.insert(child, parent);
        self.entries.entry(parent).or_default();
        self.propagate(child, bytes, files, gen);
    }

    /// Remove a directory and take its subtree out of its ancestors' totals.
    pub fn remove(&mut self, key: DirKey) {
        let Some(entry) = self.entries.remove(&key) else {
            return;
        };
        self.generation += 1;
        let gen = self.generation;
        let bytes = entry.subtree_bytes as i128;
        let files = entry.subtree_files as i128;
        self.propagate(key, -bytes, -files, gen);
        self.parents.remove(&key);
    }

    /// Mark a subtree as no longer trustworthy.
    ///
    /// Called when a watch could not be established, or when the kernel dropped
    /// events. The numbers are kept -- a stale total is more useful than none,
    /// as long as the caller is told which it is.
    pub fn mark_stale(&mut self, key: DirKey) {
        let links: Vec<(DirKey, DirKey)> = self.parents.iter().map(|(c, p)| (*c, *p)).collect();
        let mut stack = vec![key];
        let mut seen = 0usize;
        // A cycle in the parent links would otherwise revisit forever.
        let limit = links.len() + 1;
        while let Some(k) = stack.pop() {
            seen += 1;
            if seen > limit {
                return;
            }
            if let Some(e) = self.entries.get_mut(&k) {
                e.state = Freshness::Stale;
            }
            for (child, parent) in &links {
                if *parent == k && *child != k {
                    stack.push(*child);
                }
            }
        }
    }

    /// Total for a subtree, with the freshness the caller must respect.
    ///
    /// Returns `None` for a directory the index has never seen, so a missing
    /// entry cannot be mistaken for an empty one.
    pub fn subtree_total(&self, key: DirKey) -> Option<(u64, u64, Freshness)> {
        self.entries
            .get(&key)
            .map(|e| (e.subtree_bytes, e.subtree_files, e.state))
    }

    /// Add a delta to every ancestor of `from`, excluding `from` itself.
    fn propagate(&mut self, from: DirKey, d_bytes: i128, d_files: i128, gen: u64) {
        let Some(parent) = self.parents.get(&from).copied() else {
            return;
        };
        // A directory that is its own parent has no ancestors. Propagating
        // would add the delta to the entry a second time, doubling it.
        if parent == from {
            return;
        }
        self.propagate_from(parent, d_bytes, d_files, gen);
    }

    /// Add a delta to `start` and every ancestor above it.
    ///
    /// Bounded: a cycle in the parent links would otherwise hang the caller,
    /// and those links come from a filesystem that may be damaged.
    fn propagate_from(&mut self, start: DirKey, d_bytes: i128, d_files: i128, gen: u64) {
        if d_bytes == 0 && d_files == 0 {
            return;
        }
        const MAX_DEPTH: usize = 256;
        let mut current = start;
        for _ in 0..MAX_DEPTH {
            let entry = self.entries.entry(current).or_default();
            entry.subtree_bytes = apply_delta(entry.subtree_bytes, d_bytes);
            entry.subtree_files = apply_delta(entry.subtree_files, d_files);
            entry.generation = gen;

            let Some(parent) = self.parents.get(&current).copied() else {
                return;
            };
            if parent == current {
                return;
            }
            current = parent;
        }
    }
}

/// Apply a signed delta to an unsigned total, clamping at zero.
///
/// A negative result means the index and the filesystem disagree -- a file
/// removed twice, or an entry rebuilt without its ancestors. Clamping keeps a
/// wrong total from becoming an absurd one: a u64 wrapping to 18 exabytes is
/// harder to recognise as a bug than a number that is merely too small.
fn apply_delta(total: u64, delta: i128) -> u64 {
    let next = total as i128 + delta;
    if next < 0 {
        0
    } else {
        next as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOT: DirKey = (1, 1);
    const A: DirKey = (1, 10);
    const B: DirKey = (1, 11);
    const C: DirKey = (1, 12);

    /// root -> a -> b, plus root -> c
    fn tree() -> Index {
        let mut ix = Index::new();
        ix.link(A, ROOT);
        ix.link(B, A);
        ix.link(C, ROOT);
        ix
    }

    #[test]
    fn a_directorys_own_bytes_reach_the_root() {
        let mut ix = tree();
        ix.set_own(B, 1000, 2);
        assert_eq!(ix.get(B).unwrap().subtree_bytes, 1000);
        assert_eq!(ix.get(A).unwrap().subtree_bytes, 1000);
        assert_eq!(
            ix.get(ROOT).unwrap().subtree_bytes,
            1000,
            "reading the root alone must answer the whole question"
        );
    }

    #[test]
    fn siblings_both_count_towards_the_root() {
        let mut ix = tree();
        ix.set_own(B, 1000, 2);
        ix.set_own(C, 500, 1);
        assert_eq!(ix.get(ROOT).unwrap().subtree_bytes, 1500);
        assert_eq!(ix.get(ROOT).unwrap().subtree_files, 3);
        assert_eq!(
            ix.get(A).unwrap().subtree_bytes,
            1000,
            "C is not under A, so it must not appear there"
        );
    }

    #[test]
    fn an_update_applies_the_difference_not_the_new_value() {
        let mut ix = tree();
        ix.set_own(B, 1000, 2);
        ix.set_own(B, 1500, 3);
        assert_eq!(
            ix.get(ROOT).unwrap().subtree_bytes,
            1500,
            "adding the new value again would give 2500"
        );
        assert_eq!(ix.get(ROOT).unwrap().subtree_files, 3);
    }

    #[test]
    fn a_shrinking_directory_reduces_its_ancestors() {
        let mut ix = tree();
        ix.set_own(B, 1000, 2);
        ix.set_own(B, 400, 1);
        assert_eq!(ix.get(ROOT).unwrap().subtree_bytes, 400);
    }

    #[test]
    fn setting_the_same_value_twice_changes_nothing() {
        let mut ix = tree();
        ix.set_own(B, 1000, 2);
        let before = ix.get(ROOT).unwrap().subtree_bytes;
        ix.set_own(B, 1000, 2);
        assert_eq!(ix.get(ROOT).unwrap().subtree_bytes, before);
    }

    #[test]
    fn removing_a_directory_takes_its_bytes_with_it() {
        let mut ix = tree();
        ix.set_own(B, 1000, 2);
        ix.set_own(C, 500, 1);
        ix.remove(B);
        assert_eq!(ix.get(ROOT).unwrap().subtree_bytes, 500);
        assert!(ix.get(B).is_none());
    }

    #[test]
    fn a_rename_moves_the_subtree_rather_than_duplicating_it() {
        let mut ix = tree();
        ix.set_own(B, 1000, 2);
        assert_eq!(ix.get(A).unwrap().subtree_bytes, 1000);

        // mv a/b c/b
        ix.link(B, C);

        assert_eq!(ix.get(A).unwrap().subtree_bytes, 0, "left the old parent");
        assert_eq!(ix.get(C).unwrap().subtree_bytes, 1000, "arrived at the new");
        assert_eq!(
            ix.get(ROOT).unwrap().subtree_bytes,
            1000,
            "the total is unchanged: the bytes moved, they did not multiply"
        );
    }

    #[test]
    fn relinking_to_the_same_parent_is_a_no_op() {
        let mut ix = tree();
        ix.set_own(B, 1000, 2);
        ix.link(B, A);
        assert_eq!(ix.get(ROOT).unwrap().subtree_bytes, 1000);
    }

    #[test]
    fn a_new_directory_starts_unknown_not_empty() {
        let ix = Index::new();
        assert_eq!(
            ix.subtree_total(A),
            None,
            "a missing entry must not read as zero bytes"
        );
    }

    #[test]
    fn a_measured_directory_is_fresh() {
        let mut ix = tree();
        ix.set_own(B, 1000, 2);
        assert_eq!(ix.get(B).unwrap().state, Freshness::Fresh);
    }

    #[test]
    fn marking_stale_covers_the_whole_subtree() {
        let mut ix = tree();
        ix.set_own(A, 10, 1);
        ix.set_own(B, 1000, 2);
        ix.set_own(C, 500, 1);
        ix.mark_stale(A);
        assert_eq!(ix.get(A).unwrap().state, Freshness::Stale);
        assert_eq!(
            ix.get(B).unwrap().state,
            Freshness::Stale,
            "a dropped event under A invalidates B too"
        );
        assert_eq!(
            ix.get(C).unwrap().state,
            Freshness::Fresh,
            "C is not under A and stays usable"
        );
    }

    #[test]
    fn a_stale_total_is_still_returned_with_its_state() {
        let mut ix = tree();
        ix.set_own(B, 1000, 2);
        ix.mark_stale(B);
        let (bytes, _, state) = ix.subtree_total(B).expect("entry exists");
        assert_eq!(bytes, 1000, "a stale number beats no number");
        assert_eq!(state, Freshness::Stale, "as long as the caller is told");
    }

    #[test]
    fn marking_stale_on_a_cycle_does_not_hang() {
        let mut ix = Index::new();
        ix.link(A, B);
        ix.link(B, A);
        ix.mark_stale(A);
    }

    #[test]
    fn a_parent_cycle_does_not_hang() {
        let mut ix = Index::new();
        ix.link(A, B);
        ix.link(B, A);
        ix.set_own(A, 100, 1);
        // Bounded walk; reaching this line at all is the assertion.
        assert!(ix.get(A).unwrap().subtree_bytes >= 100);
    }

    #[test]
    fn a_self_parent_does_not_hang() {
        let mut ix = Index::new();
        ix.link(A, A);
        ix.set_own(A, 100, 1);
        assert_eq!(ix.get(A).unwrap().subtree_bytes, 100);
    }

    #[test]
    fn a_delta_below_zero_clamps_rather_than_wrapping() {
        assert_eq!(apply_delta(100, -500), 0, "18 exabytes would be worse");
        assert_eq!(apply_delta(100, -100), 0);
        assert_eq!(apply_delta(100, 50), 150);
    }

    #[test]
    fn deep_nesting_still_reaches_the_root() {
        let mut ix = Index::new();
        let depth = 100u64;
        for i in 1..=depth {
            ix.link((1, i + 1), (1, i));
        }
        ix.set_own((1, depth + 1), 777, 1);
        assert_eq!(
            ix.get((1, 1)).unwrap().subtree_bytes,
            777,
            "propagation must cover realistic nesting"
        );
    }

    #[test]
    fn each_update_advances_the_generation() {
        let mut ix = tree();
        ix.set_own(B, 1000, 2);
        let g1 = ix.get(ROOT).unwrap().generation;
        ix.set_own(C, 1, 1);
        let g2 = ix.get(ROOT).unwrap().generation;
        assert!(
            g2 > g1,
            "the root was touched again, so its generation moves"
        );
    }
}
