use std::path::{Path, PathBuf};

use ahash::AHashMap as HashMap;

use crate::{Stat, StatMap};

#[inline(always)]
fn depth_of(p: &Path) -> usize {
    p.components().count()
}

/// Propagate each directory's totals into its ancestors.
///
/// Only directories that are already present in the map receive contributions,
/// so no entries are created above the scan root. Every directory that was
/// enumerated is present (backends insert the directory entry before reading
/// it), hence the chain root..leaf is always complete.
///
/// Complexity: O(n log n) for the depth sort plus O(n) hash lookups; keys are
/// moved, never cloned.
pub fn rollup_child_to_parent(map: StatMap) -> StatMap {
    let n = map.len();
    if n <= 1 {
        return map;
    }
    let mut entries: Vec<(usize, PathBuf, Stat)> =
        map.into_iter().map(|(p, s)| (depth_of(&p), p, s)).collect();
    // Deepest first so a directory is complete before it is added to its parent.
    entries.sort_unstable_by_key(|e| std::cmp::Reverse(e.0));
    let (paths, mut stats): (Vec<PathBuf>, Vec<Stat>) =
        entries.into_iter().map(|(_, p, s)| (p, s)).unzip();
    let index: HashMap<&Path, usize> = paths
        .iter()
        .enumerate()
        .map(|(i, p)| (p.as_path(), i))
        .collect();
    for i in 0..n {
        // Walk up to the nearest ancestor that is present. The immediate parent
        // is the normal case; looking further keeps a subtree's totals from
        // being dropped when a link in the chain is missing, for instance an
        // unreadable directory.
        let Some(j) = paths[i]
            .ancestors()
            .skip(1)
            .find_map(|a| index.get(a).copied())
        else {
            continue;
        };
        let total = stats[i];
        stats[j].add(&total);
    }
    drop(index);
    paths.into_iter().zip(stats).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stat(files: u64) -> Stat {
        Stat {
            logical: files * 10,
            physical: files * 16,
            files,
        }
    }

    #[test]
    fn totals_propagate_up_to_root_only() {
        let root = PathBuf::from("/scan/root");
        let mut m: StatMap = HashMap::default();
        m.insert(root.clone(), stat(1));
        m.insert(root.join("a"), stat(2));
        m.insert(root.join("a").join("b"), stat(4));
        m.insert(root.join("c"), stat(8));
        let out = rollup_child_to_parent(m);
        assert_eq!(out.len(), 4, "no entries above the root are created");
        assert_eq!(out[&root].files, 15);
        assert_eq!(out[&root].physical, 15 * 16);
        assert_eq!(out[&root.join("a")].files, 6);
        assert_eq!(out[&root.join("a").join("b")].files, 4);
        assert_eq!(out[&root.join("c")].files, 8);
        assert!(!out.contains_key(Path::new("/scan")));
    }

    #[test]
    fn empty_and_single_are_untouched() {
        let empty: StatMap = HashMap::default();
        assert!(rollup_child_to_parent(empty).is_empty());
        let mut single: StatMap = HashMap::default();
        single.insert(PathBuf::from("/x"), stat(3));
        let out = rollup_child_to_parent(single);
        assert_eq!(out[&PathBuf::from("/x")].files, 3);
    }
}
