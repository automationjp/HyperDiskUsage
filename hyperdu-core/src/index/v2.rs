//! File identities and directory deltas for continuously monitored indexes.
//! The v1 directory snapshot API remains independent of this format.

mod persistence;
mod scan;
mod watch;
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::{OsStr, OsString},
    io,
    path::{Component, Path, PathBuf},
};

pub use scan::{observe, UpdateStats};
pub use watch::{IndexWatcher, WatchUpdate};

use super::journal::JournalCursor;

/// Notification freshness is intentionally distinct from an atomic snapshot.
/// Observed means all delivered events are applied; mmap, remote writes or
/// writes through aliases outside a watch may still require reconciliation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Freshness {
    Unknown,
    Stale,
    Observed,
}

/// Full native file identity: NTFS/ReFS retain all 128 file-ID bits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EntryId {
    pub volume: u64,
    pub object: u128,
}

/// A namespace entry, preserving Unix bytes or Windows UTF-16.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct LinkKey {
    pub parent: EntryId,
    pub name: OsString,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Directory,
    Other,
}

/// Regular-file totals. Symlinks, special files and directory storage are
/// excluded. Hardlinks contribute once to their smallest LinkKey.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Totals {
    pub logical: u64,
    pub physical: u64,
    pub files: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ObservedEntry {
    pub id: EntryId,
    pub kind: EntryKind,
    pub logical: u64,
    pub physical: u64,
}

#[derive(Debug)]
struct Directory {
    link: Option<LinkKey>,
    children: BTreeMap<OsString, EntryId>,
    own: Totals,
    total: Totals,
}

#[derive(Debug)]
struct FileRecord {
    logical: u64,
    physical: u64,
    links: BTreeSet<LinkKey>,
}

impl FileRecord {
    fn contribution(&self) -> Option<(EntryId, Totals)> {
        self.links.first().map(|link| {
            (
                link.parent,
                Totals {
                    logical: self.logical,
                    physical: self.physical,
                    files: 1,
                },
            )
        })
    }
}

/// A per-entry store whose ordinary updates touch one object and its ancestors.
/// A failed update leaves the store stale; callers must rebuild before saving
/// a new journal cursor. Saved data always loads stale.
#[derive(Debug)]
pub struct PersistentIndex {
    root: EntryId,
    dirs: BTreeMap<EntryId, Directory>,
    files: BTreeMap<EntryId, FileRecord>,
    freshness: Freshness,
    cursor: Option<JournalCursor>,
    valid: bool,
}

impl PersistentIndex {
    pub fn new(root: EntryId) -> Self {
        let mut dirs = BTreeMap::new();
        dirs.insert(
            root,
            Directory {
                link: None,
                children: BTreeMap::new(),
                own: Totals::default(),
                total: Totals::default(),
            },
        );
        Self {
            root,
            dirs,
            files: BTreeMap::new(),
            freshness: Freshness::Unknown,
            cursor: None,
            valid: true,
        }
    }

    pub fn root(&self) -> EntryId {
        self.root
    }

    pub fn cursor(&self) -> Option<JournalCursor> {
        self.cursor
    }

    pub fn directory_count(&self) -> usize {
        self.dirs.len()
    }

    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    pub fn total(&self, id: EntryId) -> Option<(Totals, Freshness)> {
        self.dirs.get(&id).map(|dir| (dir.total, self.freshness))
    }

    pub fn invalidate(&mut self) {
        self.freshness = Freshness::Stale;
    }

    /// Called only after the native service validates and drains a continuous
    /// event interval. Observed describes advisory OS observation, not an
    /// atomic filesystem snapshot; the service periodically reconciles it.
    pub(crate) fn mark_current(&mut self, cursor: JournalCursor) -> io::Result<()> {
        if !self.valid {
            return Err(invalid("failed index update requires a rebuild"));
        }
        if cursor.volume != self.root.volume {
            return Err(invalid("journal volume does not match root"));
        }
        self.cursor = Some(cursor);
        self.freshness = Freshness::Observed;
        Ok(())
    }

    pub fn lookup(&self, parent: EntryId, name: &OsStr) -> Option<EntryId> {
        self.dirs.get(&parent)?.children.get(name).copied()
    }

    pub fn is_directory(&self, id: EntryId) -> bool {
        self.dirs.contains_key(&id)
    }

    pub fn links(&self, id: EntryId) -> Vec<LinkKey> {
        if let Some(file) = self.files.get(&id) {
            file.links.iter().cloned().collect()
        } else {
            self.dirs
                .get(&id)
                .and_then(|dir| dir.link.clone())
                .into_iter()
                .collect()
        }
    }

    /// Resolve a directory through parent links without touching the filesystem.
    pub fn relative_path(&self, id: EntryId) -> Option<PathBuf> {
        let mut id = id;
        let mut names = Vec::new();
        while id != self.root {
            let link = self.dirs.get(&id)?.link.as_ref()?;
            names.push(link.name.as_os_str());
            id = link.parent;
            if names.len() > self.dirs.len() {
                return None;
            }
        }
        let mut result = PathBuf::new();
        for name in names.into_iter().rev() {
            result.push(name);
        }
        Some(result)
    }

    pub fn directory_at(&self, relative: &Path) -> Option<EntryId> {
        let mut id = self.root;
        for component in relative.components() {
            let Component::Normal(name) = component else {
                return None;
            };
            id = self.lookup(id, name)?;
            if !self.dirs.contains_key(&id) {
                return None;
            }
        }
        Some(id)
    }

    /// Reconcile one observed namespace entry. Reusing a directory identity at
    /// a new name moves its existing subtree without reading descendants.
    pub fn upsert(&mut self, key: LinkKey, observed: ObservedEntry) -> io::Result<()> {
        let result = self.upsert_inner(key, observed);
        if result.is_err() {
            self.valid = false;
        }
        result
    }

    fn upsert_inner(&mut self, key: LinkKey, observed: ObservedEntry) -> io::Result<()> {
        self.invalidate();
        validate_name(&key.name)?;
        if !self.dirs.contains_key(&key.parent) {
            return Err(invalid("missing parent directory"));
        }
        if observed.kind != EntryKind::Other && observed.id.volume != self.root.volume {
            return Err(invalid("entry volume does not match root"));
        }
        if observed.kind == EntryKind::Other {
            return self.remove(&key, None);
        }
        if self.dirs.contains_key(&observed.id) != (observed.kind == EntryKind::Directory)
            && (self.dirs.contains_key(&observed.id) || self.files.contains_key(&observed.id))
        {
            return Err(invalid("object identity changed kind"));
        }
        if let Some(old) = self.lookup(key.parent, &key.name) {
            if old != observed.id {
                self.remove(&key, Some(old))?;
            }
        }
        match observed.kind {
            EntryKind::Directory => {
                if self.dirs.contains_key(&observed.id) {
                    self.move_directory(observed.id, key)
                } else {
                    self.dirs
                        .get_mut(&key.parent)
                        .unwrap()
                        .children
                        .insert(key.name.clone(), observed.id);
                    self.dirs.insert(
                        observed.id,
                        Directory {
                            link: Some(key),
                            children: BTreeMap::new(),
                            own: Totals::default(),
                            total: Totals::default(),
                        },
                    );
                    Ok(())
                }
            }
            EntryKind::File => {
                let old = self
                    .files
                    .get(&observed.id)
                    .and_then(FileRecord::contribution);
                let file = self.files.entry(observed.id).or_insert_with(|| FileRecord {
                    logical: observed.logical,
                    physical: observed.physical,
                    links: BTreeSet::new(),
                });
                file.logical = observed.logical;
                file.physical = observed.physical;
                file.links.insert(key.clone());
                let new = file.contribution();
                self.adjust_file(old, new)?;
                self.dirs
                    .get_mut(&key.parent)
                    .unwrap()
                    .children
                    .insert(key.name, observed.id);
                Ok(())
            }
            EntryKind::Other => unreachable!(),
        }
    }

    /// Remove a known name. A supplied identity prevents an old event from
    /// deleting a newer object that has reused the same name.
    pub fn remove(&mut self, key: &LinkKey, expected: Option<EntryId>) -> io::Result<()> {
        let result = self.remove_inner(key, expected);
        if result.is_err() {
            self.valid = false;
        }
        result
    }

    fn remove_inner(&mut self, key: &LinkKey, expected: Option<EntryId>) -> io::Result<()> {
        self.invalidate();
        validate_name(&key.name)?;
        let Some(id) = self.lookup(key.parent, &key.name) else {
            return Ok(());
        };
        if expected.is_some_and(|expected| expected != id) {
            return Err(invalid("removal identity does not match current name"));
        }
        if self.files.contains_key(&id) {
            return self.remove_file_link(id, key);
        }
        let mut order = Vec::new();
        let mut pending = vec![id];
        while let Some(id) = pending.pop() {
            let dir = self
                .dirs
                .get(&id)
                .ok_or_else(|| invalid("missing subtree directory"))?;
            order.push(id);
            pending.extend(
                dir.children
                    .values()
                    .filter(|id| self.dirs.contains_key(id))
                    .copied(),
            );
        }
        for id in order.into_iter().rev() {
            let children: Vec<_> = self.dirs[&id]
                .children
                .iter()
                .map(|(name, child)| (name.clone(), *child))
                .collect();
            for (name, child) in children {
                self.remove_file_link(child, &LinkKey { parent: id, name })?;
            }
            let dir = self.dirs.remove(&id).unwrap();
            if dir.total != Totals::default() {
                return Err(invalid("nonzero removed subtree"));
            }
            let link = dir.link.ok_or_else(|| invalid("cannot remove root"))?;
            self.dirs
                .get_mut(&link.parent)
                .ok_or_else(|| invalid("missing removed parent"))?
                .children
                .remove(&link.name);
        }
        Ok(())
    }

    fn remove_file_link(&mut self, id: EntryId, key: &LinkKey) -> io::Result<()> {
        let file = self
            .files
            .get_mut(&id)
            .ok_or_else(|| invalid("missing linked file"))?;
        let old = file.contribution();
        if !file.links.remove(key) {
            return Err(invalid("missing file link"));
        }
        let new = file.contribution();
        self.adjust_file(old, new)?;
        if self.files[&id].links.is_empty() {
            self.files.remove(&id);
        }
        self.dirs
            .get_mut(&key.parent)
            .ok_or_else(|| invalid("missing file parent"))?
            .children
            .remove(&key.name);
        Ok(())
    }

    fn move_directory(&mut self, id: EntryId, key: LinkKey) -> io::Result<()> {
        let old = self.dirs[&id]
            .link
            .clone()
            .ok_or_else(|| invalid("cannot move root"))?;
        if old == key {
            return Ok(());
        }
        let mut ancestor = key.parent;
        loop {
            if ancestor == id {
                return Err(invalid("directory move creates a cycle"));
            }
            match self.dirs.get(&ancestor).and_then(|dir| dir.link.as_ref()) {
                Some(link) => ancestor = link.parent,
                None if ancestor == self.root => break,
                None => return Err(invalid("missing move ancestor")),
            }
        }
        let total = self.dirs[&id].total;
        let mut updates = BTreeMap::new();
        self.collect_delta(old.parent, total, -1, false, &mut updates)?;
        self.collect_delta(key.parent, total, 1, false, &mut updates)?;
        self.apply_deltas(updates)?;
        self.dirs
            .get_mut(&old.parent)
            .unwrap()
            .children
            .remove(&old.name);
        self.dirs
            .get_mut(&key.parent)
            .unwrap()
            .children
            .insert(key.name.clone(), id);
        self.dirs.get_mut(&id).unwrap().link = Some(key);
        Ok(())
    }

    fn adjust_file(
        &mut self,
        old: Option<(EntryId, Totals)>,
        new: Option<(EntryId, Totals)>,
    ) -> io::Result<()> {
        let mut updates = BTreeMap::new();
        if let Some((id, total)) = old {
            self.collect_delta(id, total, -1, true, &mut updates)?;
        }
        if let Some((id, total)) = new {
            self.collect_delta(id, total, 1, true, &mut updates)?;
        }
        self.apply_deltas(updates)
    }

    fn collect_delta(
        &self,
        mut id: EntryId,
        total: Totals,
        sign: i128,
        mut own: bool,
        updates: &mut BTreeMap<EntryId, (Delta, Delta)>,
    ) -> io::Result<()> {
        let mut depth = 0;
        loop {
            let dir = self
                .dirs
                .get(&id)
                .ok_or_else(|| invalid("missing delta ancestor"))?;
            let update = updates.entry(id).or_default();
            update.1.add(total, sign);
            if own {
                update.0.add(total, sign);
            }
            let Some(link) = &dir.link else {
                if id != self.root {
                    return Err(invalid("disconnected delta ancestor"));
                }
                break;
            };
            id = link.parent;
            own = false;
            depth += 1;
            if depth > self.dirs.len() {
                return Err(invalid("cyclic delta ancestors"));
            }
        }
        Ok(())
    }

    fn apply_deltas(&mut self, updates: BTreeMap<EntryId, (Delta, Delta)>) -> io::Result<()> {
        let mut checked = Vec::with_capacity(updates.len());
        for (id, (own, subtree)) in updates {
            let dir = &self.dirs[&id];
            checked.push((id, own.apply(dir.own)?, subtree.apply(dir.total)?));
        }
        for (id, own, total) in checked {
            let dir = self.dirs.get_mut(&id).unwrap();
            dir.own = own;
            dir.total = total;
        }
        Ok(())
    }
}

#[derive(Default)]
struct Delta {
    logical: i128,
    physical: i128,
    files: i128,
}

impl Delta {
    fn add(&mut self, total: Totals, sign: i128) {
        self.logical += sign * i128::from(total.logical);
        self.physical += sign * i128::from(total.physical);
        self.files += sign * i128::from(total.files);
    }
    fn apply(self, total: Totals) -> io::Result<Totals> {
        let checked = |value: u64, delta: i128| {
            u64::try_from(i128::from(value) + delta)
                .map_err(|_| invalid("aggregate overflow or underflow"))
        };
        Ok(Totals {
            logical: checked(total.logical, self.logical)?,
            physical: checked(total.physical, self.physical)?,
            files: checked(total.files, self.files)?,
        })
    }
}

fn validate_name(name: &OsStr) -> io::Result<()> {
    let mut components = Path::new(name).components();
    if !matches!(components.next(), Some(Component::Normal(actual)) if actual == name)
        || components.next().is_some()
    {
        return Err(invalid("entry name must be one normal component"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        if name.as_bytes().contains(&0) {
            return Err(invalid("NUL in entry name"));
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        if name.encode_wide().any(|ch| matches!(ch, 0 | 58 | 47 | 92)) {
            return Err(invalid("invalid Windows entry name"));
        }
    }
    Ok(())
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests;
