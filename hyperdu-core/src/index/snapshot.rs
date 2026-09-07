//! Explicit Linux snapshots backed by the existing exact scanner. No per-file
//! database writes and no watcher claim: a snapshot is always stale.
use std::{
    fs,
    os::unix::fs::MetadataExt,
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use anyhow::{bail, Context, Result};

use super::{DirEntry, DirKey, Freshness, HashMap, Index};

impl Index {
    /// Scan one Linux root using the default physical-size/hardlink semantics.
    /// Refuses cancellation, scan errors, identity aliases and inconsistent
    /// rollups. The returned snapshot is Stale because the scan is not atomic.
    /// No database is changed by this function.
    pub fn scan_snapshot(root: &Path, cancel: Arc<AtomicBool>) -> Result<(DirKey, Self)> {
        let root = root.canonicalize().context("resolve snapshot root")?;
        let before = directory_key(&root)?;
        let options = crate::Options {
            cancel,
            ..Default::default()
        };
        if options.cancel.load(Ordering::Relaxed) {
            bail!("snapshot cancelled");
        }
        let totals = crate::scan_directory(&root, &options)?;
        if options.cancel.load(Ordering::Relaxed) {
            bail!("snapshot cancelled");
        }
        if options.error_count.load(Ordering::Relaxed) != 0 {
            bail!("incomplete scan; snapshot not saved");
        }
        if before != directory_key(&root)? {
            bail!("root identity changed during scan");
        }
        let mut identities = HashMap::default();
        let mut index = Index::new();
        // Populate subtree totals directly, avoiding one O(depth) rollup per
        // directory (and the incremental updater's depth guard).
        for (path, stat) in &totals {
            if options.cancel.load(Ordering::Relaxed) {
                bail!("snapshot cancelled");
            }
            if !path.starts_with(&root) {
                bail!("scan escaped snapshot root");
            }
            let key = directory_key(path)?;
            if index
                .entries
                .insert(
                    key,
                    DirEntry {
                        own_bytes: stat.physical,
                        own_files: stat.files,
                        subtree_bytes: stat.physical,
                        subtree_files: stat.files,
                        generation: 1,
                        state: Freshness::Stale,
                    },
                )
                .is_some()
            {
                bail!("directory identity alias; use separate roots for bind mounts");
            }
            identities.insert(path.clone(), key);
        }
        if identities.get(&root) != Some(&before) {
            bail!("root missing from scan");
        }
        for (path, &key) in &identities {
            if path == &root {
                continue;
            }
            let parent = path
                .parent()
                .and_then(|p| identities.get(p))
                .context("missing snapshot parent")?;
            let child = index.entries[&key];
            let entry = index
                .entries
                .get_mut(parent)
                .context("missing parent entry")?;
            entry.own_bytes = entry
                .own_bytes
                .checked_sub(child.subtree_bytes)
                .context("inconsistent byte totals")?;
            entry.own_files = entry
                .own_files
                .checked_sub(child.subtree_files)
                .context("inconsistent file totals")?;
            index.parents.insert(key, *parent);
        }
        index.generation = 1;
        index.validate_loaded()?;
        Ok((before, index))
    }
}

/// Stable directory identity within a mounted filesystem. Does not follow a
/// final symlink; callers canonicalize their explicit root first.
pub(super) fn directory_key(path: &Path) -> Result<DirKey> {
    let metadata =
        fs::symlink_metadata(path).with_context(|| format!("stat {}", path.display()))?;
    if !metadata.is_dir() {
        bail!("not a directory: {}", path.display());
    }
    Ok((metadata.dev(), metadata.ino()))
}
