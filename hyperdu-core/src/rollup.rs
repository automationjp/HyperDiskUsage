use crate::StatMap;
use ahash::AHashMap as HashMap;
use std::path::{Path, PathBuf};

#[inline(always)]
fn depth_of(p: &Path) -> usize {
    p.components().count()
}

pub fn rollup_child_to_parent(mut merged: StatMap) -> StatMap {
    let mut by_depth: HashMap<usize, Vec<PathBuf>> = HashMap::default();
    let mut maxd = 0usize;
    for p in merged.keys() {
        let d = depth_of(p);
        by_depth.entry(d).or_default().push(p.clone());
        if d > maxd {
            maxd = d;
        }
    }
    for d in (1..=maxd).rev() {
        if let Some(paths) = by_depth.get(&d) {
            for p in paths {
                if let Some(parent) = p.parent() {
                    if let Some(stat) = merged.get(p).copied() {
                        let e = merged.entry(parent.to_path_buf()).or_default();
                        e.logical += stat.logical;
                        e.physical += stat.physical;
                        e.files += stat.files;
                    }
                }
            }
        }
    }
    merged
}
