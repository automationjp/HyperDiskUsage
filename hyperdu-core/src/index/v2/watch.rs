//! A journal cursor and its updates become publishable only after an empty
//! catch-up barrier. Gaps discard the working session and rebuild explicitly.
use std::{
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

use super::*;
use crate::index::journal::{self, Batch, Change, NativeJournal};

/// Result of one bounded replay. A busy source may need another poll before a
/// snapshot can be published.
#[derive(Clone, Copy, Debug, Default)]
pub struct WatchUpdate {
    pub caught_up: bool,
    pub rebuilt: bool,
    pub notifications: u64,
    pub stats: UpdateStats,
}

/// Foreground native monitoring; no operating-system service is installed.
pub struct IndexWatcher {
    root: PathBuf,
    index: PersistentIndex,
    source: Box<dyn NativeJournal>,
    reconcile_interval: Duration,
    last_rebuild: Instant,
}

impl IndexWatcher {
    /// Start before the initial enumeration, or validate a persistent journal
    /// resume position. Inotify/fanotify always require a baseline on restart.
    pub fn start(
        root: &Path,
        saved: Option<PersistentIndex>,
        cancel: &AtomicBool,
    ) -> io::Result<Self> {
        let root = root.canonicalize()?;
        let root_id = observe(&root)?.id;
        if saved.as_ref().is_some_and(|index| index.root() != root_id) {
            return Err(invalid(
                "saved index belongs to another root; refresh explicitly",
            ));
        }
        let (source, resumed) = journal::open(
            &root,
            root_id,
            saved.as_ref().and_then(|index| index.cursor()),
        )?;
        Self::with_source(root, saved, source, resumed, cancel)
    }

    fn with_source(
        root: PathBuf,
        saved: Option<PersistentIndex>,
        mut source: Box<dyn NativeJournal>,
        resumed: bool,
        cancel: &AtomicBool,
    ) -> io::Result<Self> {
        let cursor = source.cursor();
        let mut index = if resumed {
            let index = saved.ok_or_else(|| invalid("resume requires a saved index"))?;
            if index.cursor() != Some(cursor) {
                return Err(invalid("source resumed a different cursor"));
            }
            index
        } else {
            let (mut index, _) = scan::scan(&root, cancel, Some(source.as_mut()))?;
            index.cursor = Some(cursor);
            index
        };
        index.invalidate();
        Ok(Self {
            root,
            index,
            source,
            reconcile_interval: Duration::from_secs(900),
            last_rebuild: Instant::now(),
        })
    }

    pub fn index(&self) -> &PersistentIndex {
        &self.index
    }

    /// OS notifications are advisory (for example mmap and remote changes may
    /// not be reported). A bounded periodic reconciliation remains mandatory.
    pub fn set_reconcile_interval(&mut self, interval: Duration) -> io::Result<()> {
        if interval < Duration::from_secs(1) || interval > Duration::from_secs(86400) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "reconcile interval must be between one second and one day",
            ));
        }
        self.reconcile_interval = interval;
        Ok(())
    }

    pub fn poll(&mut self, cancel: &AtomicBool) -> io::Result<WatchUpdate> {
        scan::check_cancel(cancel)?;
        if self.last_rebuild.elapsed() >= self.reconcile_interval {
            return self.rebuild(cancel);
        }
        match self.drain(cancel) {
            Ok(update) => Ok(update),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                self.index.invalidate();
                Err(error)
            }
            Err(error) => {
                self.index.invalidate();
                log::warn!("index journal invalidated: {error}; rebuilding");
                self.rebuild(cancel)
            }
        }
    }

    fn rebuild(&mut self, cancel: &AtomicBool) -> io::Result<WatchUpdate> {
        self.index.invalidate();
        let root = observe(&self.root)?;
        if root.id != self.index.root || root.kind != EntryKind::Directory {
            return Err(invalid("monitored root was replaced; refresh explicitly"));
        }
        let (mut source, _) = journal::open(&self.root, root.id, None)?;
        let start = source.cursor();
        let (mut index, stats) = scan::scan(&self.root, cancel, Some(source.as_mut()))?;
        index.cursor = Some(start);
        self.index = index;
        self.source = source;
        self.last_rebuild = Instant::now();
        let mut update = self.drain(cancel)?;
        update.rebuilt = true;
        update.stats.observed_entries += stats.observed_entries;
        update.stats.directories_read += stats.directories_read;
        Ok(update)
    }

    fn drain(&mut self, cancel: &AtomicBool) -> io::Result<WatchUpdate> {
        self.index.invalidate();
        let mut update = WatchUpdate::default();
        for _ in 0..64 {
            scan::check_cancel(cancel)?;
            let batch = self.source.poll()?;
            self.validate_batch(&batch)?;
            update.notifications += batch.changes.len() as u64;
            let empty = batch.changes.is_empty();
            let caught_up = batch.caught_up;
            let next = batch.next;
            self.apply_changes(batch.changes, cancel, &mut update.stats)?;
            self.index.cursor = Some(next);
            if caught_up && empty {
                let root = observe(&self.root)?;
                if root.kind != EntryKind::Directory || root.id != self.index.root {
                    return Err(invalid("monitored root identity changed"));
                }
                self.index.mark_current(next)?;
                update.caught_up = true;
                return Ok(update);
            }
        }
        Ok(update)
    }

    fn validate_batch(&self, batch: &Batch) -> io::Result<()> {
        if self.index.cursor != Some(batch.from)
            || batch.from.kind != batch.next.kind
            || batch.from.epoch != batch.next.epoch
            || batch.from.volume != batch.next.volume
            || batch.next.volume != self.index.root.volume
            || batch.next.position < batch.from.position
        {
            return Err(invalid(
                "journal gap, incarnation change or cursor regression",
            ));
        }
        Ok(())
    }

    fn apply_changes(
        &mut self,
        changes: Vec<Change>,
        cancel: &AtomicBool,
        stats: &mut UpdateStats,
    ) -> io::Result<()> {
        let mut keys = BTreeMap::new();
        let mut objects = BTreeSet::new();
        for change in changes {
            match change {
                Change::Reset(reason) => {
                    return Err(io::Error::new(io::ErrorKind::InvalidData, reason))
                }
                Change::Entry { key, subtree } => {
                    validate_name(&key.name)?;
                    *keys.entry(key).or_insert(false) |= subtree;
                }
                Change::RelativePath { path, subtree } => {
                    if path.as_os_str().is_empty() {
                        self.index.sync_subtree(
                            &self.root,
                            &self.root,
                            self.index.root,
                            Some(self.source.as_mut()),
                            cancel,
                            stats,
                        )?;
                    } else {
                        let (key, recursive) = self.path_target(&path)?;
                        *keys.entry(key).or_insert(false) |= subtree || recursive;
                    }
                }
                Change::ObjectLinks(id) => {
                    objects.insert(id);
                }
            }
        }
        // Observe each touched name once. Directories are relocated before
        // deletions so an old rename name cannot destroy its known descendants.
        let mut pending = Vec::new();
        for (key, subtree) in keys {
            scan::check_cancel(cancel)?;
            let path = self.path_for(&key);
            let observed = observe_optional(path.as_deref(), stats)?;
            pending.push((key, subtree, path, observed));
        }
        for (key, subtree, _, observed) in &pending {
            if observed.is_some_and(|entry| entry.kind == EntryKind::Directory) {
                self.index.apply_observed(
                    key.clone(),
                    *observed,
                    &self.root,
                    *subtree,
                    self.source.as_mut(),
                    cancel,
                    stats,
                )?;
            }
        }
        for (key, subtree, old_path, observed) in pending {
            scan::check_cancel(cancel)?;
            if observed.is_some_and(|entry| entry.kind == EntryKind::Directory) {
                continue;
            }
            if self.path_for(&key) != old_path {
                // A parent moved earlier in this batch. Its old lookup no
                // longer describes this file; resolve through the new parent.
                self.index.refresh_entry(
                    key,
                    &self.root,
                    subtree,
                    self.source.as_mut(),
                    cancel,
                    stats,
                )?;
            } else if old_path.is_some() {
                self.index.apply_observed(
                    key,
                    observed,
                    &self.root,
                    subtree,
                    self.source.as_mut(),
                    cancel,
                    stats,
                )?;
            }
        }
        for id in objects {
            self.reconcile_links(id, cancel, stats)?;
        }
        Ok(())
    }

    fn path_for(&self, key: &LinkKey) -> Option<PathBuf> {
        self.index
            .relative_path(key.parent)
            .map(|parent| self.root.join(parent).join(&key.name))
    }

    fn path_target(&self, relative: &Path) -> io::Result<(LinkKey, bool)> {
        let mut components = relative.components().peekable();
        let mut parent = self.index.root;
        while let Some(component) = components.next() {
            let Component::Normal(name) = component else {
                return Err(invalid("journal path escapes root"));
            };
            validate_name(name)?;
            let key = LinkKey {
                parent,
                name: name.to_os_string(),
            };
            if components.peek().is_none() {
                return Ok((key, false));
            }
            match self
                .index
                .lookup(parent, name)
                .filter(|id| self.index.is_directory(*id))
            {
                Some(id) => parent = id,
                None => return Ok((key, true)), // newly arrived subtree
            }
        }
        Err(invalid("empty journal path"))
    }

    #[cfg(windows)]
    fn reconcile_links(
        &mut self,
        id: EntryId,
        cancel: &AtomicBool,
        stats: &mut UpdateStats,
    ) -> io::Result<()> {
        let old = self.index.links(id);
        let mut seen = BTreeSet::new();
        for relative in journal::windows::hardlinks_for_id(&self.root, id)? {
            let (key, recursive) = self.path_target(&relative)?;
            self.index.refresh_entry(
                key.clone(),
                &self.root,
                recursive,
                self.source.as_mut(),
                cancel,
                stats,
            )?;
            if self.index.lookup(key.parent, &key.name) == Some(id) {
                seen.insert(key);
            }
        }
        for key in old {
            if !seen.contains(&key) {
                self.index.remove(&key, Some(id))?;
            }
        }
        Ok(())
    }

    #[cfg(not(windows))]
    fn reconcile_links(
        &mut self,
        _id: EntryId,
        _cancel: &AtomicBool,
        _stats: &mut UpdateStats,
    ) -> io::Result<()> {
        Err(invalid(
            "unexpected hardlink enumeration request from native journal",
        ))
    }
}

fn observe_optional(
    path: Option<&Path>,
    stats: &mut UpdateStats,
) -> io::Result<Option<ObservedEntry>> {
    let Some(path) = path else {
        return Ok(None);
    };
    stats.observed_entries += 1;
    match observe(path) {
        Ok(observed) => Ok(Some(observed)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}
#[cfg(test)]
#[path = "watch_tests.rs"]
mod tests;
