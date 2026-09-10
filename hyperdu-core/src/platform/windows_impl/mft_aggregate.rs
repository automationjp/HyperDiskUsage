//! Aggregate bytes by parent record before materializing directory paths.
//! The legacy reader remains the accounting oracle, including damaged-parent
//! handling and the 255-component path limit.
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
};

use super::{mft::ROOT_RECORD, mft_reader::Entry};

struct Paths<'a> {
    records: HashMap<u64, &'a Entry>,
    resolved: HashMap<u64, Option<(String, usize)>>,
}

impl<'a> Paths<'a> {
    fn new(entries: &'a [Entry]) -> Self {
        Self {
            // Match paths_for: the last entry for an ID supplies its name/parent.
            records: entries.iter().map(|entry| (entry.record, entry)).collect(),
            resolved: HashMap::from([(ROOT_RECORD, Some((String::new(), 0)))]),
        }
    }

    fn valid(&mut self, record: u64) -> bool {
        if let Some(path) = self.resolved.get(&record) {
            return path.is_some();
        }
        let mut chain: Vec<&Entry> = Vec::new();
        let mut current = record;
        let (mut path, mut depth) = loop {
            if let Some(cached) = self.resolved.get(&current) {
                if let Some(path) = cached {
                    break path.clone();
                }
                for entry in chain {
                    self.resolved.insert(entry.record, None);
                }
                return false;
            }
            // Do not poison ancestors when only this requested path is too
            // deep: a shallower ancestor can still fit the legacy depth bound.
            if chain.len() == 256 {
                self.resolved.insert(record, None);
                return false;
            }
            if chain.iter().any(|entry| entry.record == current) {
                for entry in chain {
                    self.resolved.insert(entry.record, None);
                }
                return false;
            }
            let Some(entry) = self.records.get(&current).copied() else {
                self.resolved.insert(current, None);
                for entry in chain {
                    self.resolved.insert(entry.record, None);
                }
                return false;
            };
            chain.push(entry);
            current = entry.parent;
        };
        for entry in chain.into_iter().rev() {
            if depth > 0 {
                path.push('\\');
            }
            path.push_str(&entry.name);
            depth += 1;
            self.resolved
                .insert(entry.record, (depth < 256).then(|| (path.clone(), depth)));
        }
        self.resolved.get(&record).is_some_and(Option::is_some)
    }

    fn get(&self, record: u64) -> &str {
        &self.resolved[&record]
            .as_ref()
            .expect("validated parent path")
            .0
    }
}

fn join_path(prefix: &str, rest: &str) -> PathBuf {
    if rest.is_empty() {
        return PathBuf::from(prefix);
    }
    let mut path = String::with_capacity(prefix.len() + rest.len() + 1);
    path.push_str(prefix);
    if !prefix.ends_with('\\') && !prefix.ends_with('/') {
        path.push('\\');
    }
    path.push_str(rest);
    PathBuf::from(path)
}

/// Project only directories and files reachable through listable ancestors.
/// None aborts the entire projection; false preserves the directory's empty row.
pub(crate) fn to_visible_stat_map(
    entries: &[Entry],
    root_prefix: &str,
    count_hardlinks: bool,
    compute_physical: bool,
    mut listable: impl FnMut(&std::path::Path) -> Option<bool>,
) -> Option<crate::StatMap> {
    let mut paths = Paths::new(entries);
    let mut children: HashMap<u64, Vec<&Entry>> = HashMap::new();
    for entry in entries.iter().filter(|entry| entry.is_directory) {
        children.entry(entry.parent).or_default().push(entry);
    }
    let mut visible = HashSet::from([ROOT_RECORD]);
    let mut readable = HashSet::new();
    let mut pending = vec![ROOT_RECORD];
    while let Some(record) = pending.pop() {
        if !listable(&join_path(root_prefix, paths.get(record)))? {
            continue;
        }
        readable.insert(record);
        if let Some(dirs) = children.get(&record) {
            for entry in dirs {
                if paths.valid(entry.record) && visible.insert(entry.record) {
                    pending.push(entry.record);
                }
            }
        }
    }
    // The reader emits only one name per record. A hidden chosen hardlink
    // cannot establish whether some other, unrepresented name is visible.
    if entries.iter().any(|entry| {
        !entry.is_directory && entry.hard_link_count > 1 && !readable.contains(&entry.parent)
    }) {
        return None;
    }
    let projected: Vec<_> = entries
        .iter()
        .filter(|entry| {
            if entry.is_directory {
                visible.contains(&entry.record)
            } else {
                readable.contains(&entry.parent)
            }
        })
        .cloned()
        .collect();
    Some(to_stat_map(
        &projected,
        root_prefix,
        count_hardlinks,
        compute_physical,
    ))
}
pub(crate) fn to_stat_map(
    entries: &[Entry],
    root_prefix: &str,
    count_hardlinks: bool,
    compute_physical: bool,
) -> crate::StatMap {
    let mut paths = Paths::new(entries);
    let mut totals: HashMap<u64, crate::Stat> = HashMap::new();
    let mut counted = HashSet::new();
    totals.insert(ROOT_RECORD, crate::Stat::default());
    for entry in entries.iter().filter(|entry| entry.is_directory) {
        if paths.valid(entry.record) {
            totals.entry(entry.record).or_default();
        }
    }
    for entry in entries.iter().filter(|entry| !entry.is_directory) {
        if !paths.valid(entry.parent) {
            continue;
        }
        // An orphan must not consume an ID that a later valid hardlink uses.
        if !count_hardlinks && entry.hard_link_count > 1 && !counted.insert(entry.record) {
            continue;
        }
        let total = totals.entry(entry.parent).or_default();
        total.files += 1;
        total.logical += entry.sizes.real_size;
        total.physical += if compute_physical {
            entry.sizes.allocated_size
        } else {
            entry.sizes.real_size
        };
    }
    let mut map = crate::StatMap::default();
    for (record, total) in totals {
        // Distinct IDs can have equal paths; preserve the legacy merged total.
        map.entry(join_path(root_prefix, paths.get(record)))
            .or_default()
            .add(&total);
    }
    map
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::platform::windows_impl::{
        mft::DataSizes,
        mft_reader::{self, SizeSource},
    };

    fn entry(id: u64, parent: u64, name: &str, directory: bool) -> Entry {
        Entry {
            record: id,
            parent,
            name: name.into(),
            is_directory: directory,
            sizes: DataSizes {
                real_size: id + 1,
                allocated_size: id * 4096,
            },
            hard_link_count: 1,
            data_flags: 0,
            size_source: SizeSource::default(),
        }
    }

    #[test]
    fn visibility_denied_ancestor_keeps_empty_boundary_and_skips_descendants() {
        let entries = vec![
            entry(17, 16, "nested", true),
            entry(18, 17, "hidden", false),
            entry(19, 16, "resident", false),
            entry(16, ROOT_RECORD, "denied", true),
            entry(20, ROOT_RECORD, "visible", false),
        ];
        let mut probes = Vec::new();
        let map = to_visible_stat_map(&entries, r"C:\", false, true, |path| {
            probes.push(path.to_path_buf());
            Some(path != std::path::Path::new(r"C:\denied"))
        })
        .unwrap();
        assert_eq!(map.len(), 2);
        assert_eq!(map[&PathBuf::from(r"C:\denied")].files, 0);
        assert_eq!(map[&PathBuf::from(r"C:\")].files, 1);
        assert_eq!(probes, [PathBuf::from(r"C:\"), PathBuf::from(r"C:\denied")]);
    }
    #[test]
    fn visibility_unknown_failure_aborts_and_denied_root_stays_empty() {
        let entries = vec![entry(16, ROOT_RECORD, "d", true), entry(17, 16, "f", false)];
        assert!(to_visible_stat_map(&entries, r"C:\", false, true, |_| None).is_none());
        let mut probes = 0;
        let denied = to_visible_stat_map(&entries, r"C:\", false, true, |_| {
            probes += 1;
            Some(false)
        })
        .unwrap();
        assert_eq!(probes, 1);
        assert_eq!(denied.len(), 1);
        assert_eq!(denied[&PathBuf::from(r"C:\")].files, 0);
    }

    #[test]
    fn visibility_removed_hardlink_declines_even_with_a_represented_visible_alias() {
        let mut hidden = entry(18, 16, "hidden", false);
        hidden.hard_link_count = 2;
        let mut alias = hidden.clone();
        alias.parent = ROOT_RECORD;
        alias.name = "visible-alias".into();
        let entries = vec![entry(16, ROOT_RECORD, "denied", true), hidden, alias];
        assert!(to_visible_stat_map(&entries, r"C:\", false, true, |path| {
            Some(path != std::path::Path::new(r"C:\denied"))
        })
        .is_none());
    }

    #[test]
    fn visibility_all_readable_matches_existing_accounting_and_ignores_orphans() {
        let entries = vec![
            entry(17, 16, "child", true),
            entry(18, 17, "f", false),
            entry(16, ROOT_RECORD, "parent", true),
            entry(19, 9999, "orphan", true),
        ];
        let mut probes = Vec::new();
        let actual = to_visible_stat_map(&entries, r"C:\", false, true, |path| {
            probes.push(path.to_path_buf());
            Some(true)
        })
        .unwrap();
        assert_eq!(
            values(actual),
            values(to_stat_map(&entries, r"C:\", false, true))
        );
        assert_eq!(
            probes,
            [
                PathBuf::from(r"C:\"),
                PathBuf::from(r"C:\parent"),
                PathBuf::from(r"C:\parent\child"),
            ]
        );
    }
    fn values(map: crate::StatMap) -> BTreeMap<PathBuf, (u64, u64, u64)> {
        map.into_iter()
            .map(|(path, stat)| (path, (stat.files, stat.logical, stat.physical)))
            .collect()
    }

    fn oracle(entries: &[Entry], prefix: &str, links: bool, physical: bool) -> crate::StatMap {
        mft_reader::to_stat_map(
            entries,
            &mft_reader::paths_for(entries),
            prefix,
            links,
            physical,
        )
    }

    fn compare(entries: &[Entry]) {
        for prefix in ["C:", "C:\\", "C:/", ""] {
            for links in [false, true] {
                for physical in [false, true] {
                    assert_eq!(
                        values(to_stat_map(entries, prefix, links, physical)),
                        values(oracle(entries, prefix, links, physical))
                    );
                }
            }
        }
    }

    #[test]
    fn root_empty_unordered_and_wide_trees_match_legacy() {
        compare(&[]);
        let mut entries = vec![entry(16, ROOT_RECORD, "empty", true)];
        entries.extend((100..1100).map(|id| entry(id, 17, "file", false)));
        entries.push(entry(17, ROOT_RECORD, "directory", true));
        entries.push(entry(18, ROOT_RECORD, "root-file", false));
        compare(&entries);
    }

    #[test]
    fn duplicate_ids_path_collisions_and_nondirectory_parents_match_legacy() {
        let entries = vec![
            entry(16, ROOT_RECORD, "old-name", true),
            entry(17, ROOT_RECORD, "same", true),
            entry(18, 16, "a", false),
            entry(19, 17, "b", false),
            entry(16, ROOT_RECORD, "same", false),
            entry(20, ROOT_RECORD, "", true),
            entry(21, 20, "nested", true),
            entry(22, 21, "c", false),
            entry(23, ROOT_RECORD, "\\nested", true),
            entry(24, 23, "d", false),
        ];
        compare(&entries);
    }

    #[test]
    fn orphan_cycles_and_invalid_first_hardlink_do_not_lose_valid_bytes() {
        let mut entries = vec![
            entry(16, 9999, "orphan", true),
            entry(17, 18, "cycle-a", true),
            entry(18, 17, "cycle-b", true),
            entry(19, 19, "self", true),
            entry(20, 16, "lost", false),
            entry(21, 17, "cycle-file", false),
            entry(22, 19, "self-file", false),
            entry(23, 16, "invalid-link", false),
            entry(23, ROOT_RECORD, "valid-link", false),
            entry(23, ROOT_RECORD, "second-link", false),
        ];
        for entry in entries.iter_mut().filter(|entry| entry.record == 23) {
            entry.hard_link_count = 2;
        }
        compare(&entries);
    }

    #[test]
    fn memoization_preserves_the_255_component_limit_in_both_orders() {
        let mut entries = Vec::new();
        let mut parent = ROOT_RECORD;
        for depth in 1..=258 {
            let record = 1000 + depth;
            entries.push(entry(record, parent, "d", true));
            entries.push(entry(2000 + depth, record, "f", false));
            parent = record;
        }
        compare(&entries);
        entries.reverse();
        compare(&entries);
    }

    #[test]
    #[ignore = "CPU aggregation benchmark; run in release separately from other workloads"]
    fn benchmark_parent_id_aggregation() {
        let mut entries = Vec::new();
        let mut parent = ROOT_RECORD;
        for id in 16..48 {
            entries.push(entry(id, parent, "directory", true));
            parent = id;
        }
        entries.extend((1000..101000).map(|id| entry(id, parent, "file-name", false)));
        let expected = values(oracle(&entries, "C:\\", false, true));
        for round in 0..6 {
            for optimized in if round % 2 == 0 {
                [false, true]
            } else {
                [true, false]
            } {
                let start = std::time::Instant::now();
                let result = if optimized {
                    to_stat_map(&entries, "C:\\", false, true)
                } else {
                    oracle(&entries, "C:\\", false, true)
                };
                let elapsed = start.elapsed();
                assert_eq!(values(result), expected);
                eprintln!("mft_aggregate round={round} optimized={optimized} files=100000 depth=32 elapsed_ns={}", elapsed.as_nanos());
            }
        }
    }
}
