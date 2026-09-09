//! Ranking has to be reproducible. A `StatMap` is a hash map, so before ties
//! broke on the path, thirty same-sized directories printed a different order
//! every run of the same command.

use std::path::PathBuf;

use hyperdu_core::{top_by_physical, Stat, StatMap};

fn map_of(entries: &[(&str, u64)]) -> StatMap {
    let mut m = StatMap::default();
    for (path, physical) in entries {
        m.insert(
            PathBuf::from(path),
            Stat {
                logical: *physical,
                physical: *physical,
                files: 1,
            },
        );
    }
    m
}

fn names(v: &[(PathBuf, Stat)]) -> Vec<String> {
    v.iter().map(|(p, _)| p.display().to_string()).collect()
}

#[test]
fn ranks_by_physical_size_descending() {
    let m = map_of(&[("/a", 10), ("/b", 30), ("/c", 20)]);
    assert_eq!(names(&top_by_physical(m, 0)), ["/b", "/c", "/a"]);
}

/// The point of the change. Same input, same output, every time -- including
/// when every size is identical and the hash order is the only thing left.
#[test]
fn equal_sizes_come_out_in_a_stable_order() {
    let entries: Vec<(String, u64)> = (0..30).map(|i| (format!("/d{i:02}"), 4096)).collect();
    let refs: Vec<(&str, u64)> = entries.iter().map(|(p, s)| (p.as_str(), *s)).collect();

    let first = names(&top_by_physical(map_of(&refs), 10));
    for run in 1..8 {
        let again = names(&top_by_physical(map_of(&refs), 10));
        assert_eq!(again, first, "run {run} produced a different order");
    }
    // Ties break on the path, ascending, so the answer is also predictable.
    assert_eq!(
        first,
        ["/d00", "/d01", "/d02", "/d03", "/d04", "/d05", "/d06", "/d07", "/d08", "/d09"]
    );
}

/// The selection path (`n` smaller than the map) and the sort-everything path
/// have to agree; they are separate branches.
#[test]
fn truncated_and_full_rankings_agree_on_their_common_prefix() {
    let entries: Vec<(String, u64)> = (0..50)
        .map(|i| (format!("/d{i:02}"), (i % 7) as u64 * 1000))
        .collect();
    let refs: Vec<(&str, u64)> = entries.iter().map(|(p, s)| (p.as_str(), *s)).collect();

    let full = names(&top_by_physical(map_of(&refs), 0));
    for n in [1usize, 5, 13, 49, 50] {
        let top = names(&top_by_physical(map_of(&refs), n));
        assert_eq!(top.len(), n.min(50), "wrong length for n={n}");
        assert_eq!(top, full[..n.min(50)], "prefix disagrees for n={n}");
    }
}

#[test]
fn zero_means_every_entry() {
    let m = map_of(&[("/a", 1), ("/b", 2), ("/c", 3)]);
    assert_eq!(top_by_physical(m, 0).len(), 3);
}

#[test]
fn asking_for_more_than_there_are_returns_what_there_is() {
    let m = map_of(&[("/a", 1), ("/b", 2)]);
    assert_eq!(names(&top_by_physical(m, 99)), ["/b", "/a"]);
}

#[test]
fn an_empty_map_ranks_to_nothing() {
    assert!(top_by_physical(StatMap::default(), 5).is_empty());
    assert!(top_by_physical(StatMap::default(), 0).is_empty());
}

/// Size wins over path: a later path with more bytes still outranks an earlier
/// one with fewer. Guards against sorting by path first.
#[test]
fn size_outranks_path() {
    let m = map_of(&[("/aaa", 1), ("/zzz", 100)]);
    assert_eq!(names(&top_by_physical(m, 0)), ["/zzz", "/aaa"]);
}
