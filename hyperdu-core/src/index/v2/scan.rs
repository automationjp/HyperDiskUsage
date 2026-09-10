use std::{
    fs, io,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

use super::*;
use crate::index::journal::NativeJournal;

/// Work done by a baseline or one replay batch. Ordinary changed-file updates
/// require one observation and no directory enumeration.
#[derive(Clone, Copy, Debug, Default)]
pub struct UpdateStats {
    pub observed_entries: u64,
    pub directories_read: u64,
}

impl PersistentIndex {
    /// Exact initial observations for one filesystem, with default link and
    /// regular-file accounting. Explicit snapshots are always stale.
    pub fn scan_snapshot(root: &Path, cancel: &AtomicBool) -> io::Result<Self> {
        let (index, _) = scan(root, cancel, None)?;
        Ok(index)
    }

    pub(crate) fn refresh_entry(
        &mut self,
        key: LinkKey,
        root: &Path,
        subtree: bool,
        journal: &mut dyn NativeJournal,
        cancel: &AtomicBool,
        stats: &mut UpdateStats,
    ) -> io::Result<()> {
        check_cancel(cancel)?;
        let Some(parent) = self.relative_path(key.parent) else {
            // A volume journal also sees changes outside the indexed namespace.
            return Ok(());
        };
        let path = root.join(parent).join(&key.name);
        let observed = match observe(&path) {
            Ok(entry) => entry,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return self.remove_watched(&key, Some(journal));
            }
            Err(error) => return Err(error),
        };
        stats.observed_entries += 1;
        self.apply_observed(key, Some(observed), root, subtree, journal, cancel, stats)
    }

    // Keep observation, location, watcher, cancellation and accounting explicit at this boundary.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn apply_observed(
        &mut self,
        key: LinkKey,
        observed: Option<ObservedEntry>,
        root: &Path,
        subtree: bool,
        journal: &mut dyn NativeJournal,
        cancel: &AtomicBool,
        stats: &mut UpdateStats,
    ) -> io::Result<()> {
        let Some(observed) = observed else {
            return self.remove_watched(&key, Some(journal));
        };
        let Some(parent) = self.relative_path(key.parent) else {
            return Ok(());
        };
        let path = root.join(parent).join(&key.name);
        check_volume(self.root, observed)?;
        let new_directory =
            observed.kind == EntryKind::Directory && !self.is_directory(observed.id);
        self.retire_reused_file(root, observed, cancel, stats)?;
        self.upsert_watched(key, observed, Some(journal))?;
        if observed.kind == EntryKind::Directory && (new_directory || subtree) {
            self.sync_subtree(root, &path, observed.id, Some(journal), cancel, stats)?;
        }
        Ok(())
    }

    fn retire_reused_file(
        &mut self,
        root: &Path,
        observed: ObservedEntry,
        cancel: &AtomicBool,
        stats: &mut UpdateStats,
    ) -> io::Result<()> {
        if observed.kind != EntryKind::Directory || !self.files.contains_key(&observed.id) {
            return Ok(());
        }
        // Directory-first replay preserves renames, but a deleted Unix file's
        // inode may already belong to an arriving directory. Validate every old
        // link before removing any; a live file or another replacement requires
        // the normal fail-closed rebuild instead of discarding its accounting.
        let links = self.links(observed.id);
        for link in &links {
            check_cancel(cancel)?;
            let parent = self
                .relative_path(link.parent)
                .ok_or_else(|| invalid("missing reused file parent"))?;
            stats.observed_entries += 1;
            match observe(&root.join(parent).join(&link.name)) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Ok(current)
                    if current.id == observed.id && current.kind == EntryKind::Directory => {}
                Ok(_) => return Err(invalid("file identity reuse requires reconciliation")),
                Err(error) => return Err(error),
            }
        }
        for link in links {
            self.remove(&link, Some(observed.id))?;
        }
        Ok(())
    }

    fn upsert_watched(
        &mut self,
        key: LinkKey,
        observed: ObservedEntry,
        journal: Option<&mut dyn NativeJournal>,
    ) -> io::Result<()> {
        if self
            .lookup(key.parent, &key.name)
            .is_some_and(|old| old != observed.id || observed.kind == EntryKind::Other)
        {
            self.remove_watched(&key, journal)?;
        }
        self.upsert(key, observed)
    }

    fn remove_watched(
        &mut self,
        key: &LinkKey,
        mut journal: Option<&mut dyn NativeJournal>,
    ) -> io::Result<()> {
        // Visit only the disappearing subtree. Ordinary file updates do not
        // inspect the directory table or enumerate any filesystem directory.
        if let Some(id) = self
            .lookup(key.parent, &key.name)
            .filter(|id| self.is_directory(*id))
        {
            let mut pending = vec![id];
            while let Some(id) = pending.pop() {
                pending.extend(
                    self.dirs[&id]
                        .children
                        .values()
                        .filter(|id| self.is_directory(**id))
                        .copied(),
                );
                if let Some(source) = &mut journal {
                    source.unregister_directory(id)?;
                }
            }
        }
        self.remove(key, None)
    }
    pub(crate) fn sync_subtree(
        &mut self,
        root: &Path,
        path: &Path,
        id: EntryId,
        mut journal: Option<&mut dyn NativeJournal>,
        cancel: &AtomicBool,
        stats: &mut UpdateStats,
    ) -> io::Result<()> {
        let result = (|| {
            let mut pending = vec![(path.to_path_buf(), id)];
            while let Some((path, id)) = pending.pop() {
                check_cancel(cancel)?;
                let before = observe(&path)?;
                if before.kind != EntryKind::Directory || before.id != id {
                    return Err(invalid("directory replaced during refresh"));
                }
                if let Some(journal) = &mut journal {
                    journal.register_directory(&path, id)?;
                }
                // Register first: changes during enumeration remain in the queue.
                let entries = fs::read_dir(&path)?;
                stats.directories_read += 1;
                let mut seen = BTreeSet::new();
                for entry in entries {
                    check_cancel(cancel)?;
                    let entry = entry?;
                    let name = entry.file_name();
                    let child_path = path.join(&name);
                    let observed = observe(&child_path)?;
                    stats.observed_entries += 1;
                    check_volume(self.root, observed)?;
                    self.retire_reused_file(root, observed, cancel, stats)?;
                    seen.insert(name.clone());
                    self.upsert_watched(
                        LinkKey { parent: id, name },
                        observed,
                        journal
                            .as_mut()
                            .map(|source| &mut **source as &mut dyn NativeJournal),
                    )?;
                    if observed.kind == EntryKind::Directory {
                        pending.push((child_path, observed.id));
                    }
                }
                let removed: Vec<_> = self.dirs[&id]
                    .children
                    .keys()
                    .filter(|name| !seen.contains(*name))
                    .cloned()
                    .collect();
                for name in removed {
                    self.remove_watched(
                        &LinkKey { parent: id, name },
                        journal
                            .as_mut()
                            .map(|source| &mut **source as &mut dyn NativeJournal),
                    )?;
                }
                let after = observe(&path)?;
                if after.kind != EntryKind::Directory || after.id != id {
                    return Err(invalid("directory replaced during enumeration"));
                }
            }
            Ok(())
        })();
        if result.is_err() {
            self.valid = false;
            self.invalidate();
        }
        result
    }
}

pub(crate) fn scan(
    root: &Path,
    cancel: &AtomicBool,
    journal: Option<&mut dyn NativeJournal>,
) -> io::Result<(PersistentIndex, UpdateStats)> {
    check_cancel(cancel)?;
    let before = observe(root)?;
    if before.kind != EntryKind::Directory {
        return Err(invalid("index root must be a directory"));
    }
    let mut index = PersistentIndex::new(before.id);
    let mut stats = UpdateStats::default();
    index.sync_subtree(root, root, before.id, journal, cancel, &mut stats)?;
    if observe(root)?.id != before.id {
        return Err(invalid("index root replaced during scan"));
    }
    // An empty directory still has an observed (stale) zero, not Unknown.
    index.freshness = Freshness::Stale;
    Ok((index, stats))
}

pub(crate) fn check_cancel(cancel: &AtomicBool) -> io::Result<()> {
    if cancel.load(Ordering::Relaxed) {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "index operation cancelled",
        ))
    } else {
        Ok(())
    }
}

fn check_volume(root: EntryId, entry: ObservedEntry) -> io::Result<()> {
    if entry.kind != EntryKind::Other && entry.id.volume != root.volume {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "index monitoring requires one filesystem; index the mounted subtree separately",
        ));
    }
    Ok(())
}

/// Observe identity and sizes without substituting logical bytes for an
/// unavailable allocation count. Reparse points and symlinks are excluded.
#[cfg(unix)]
pub fn observe(path: &Path) -> io::Result<ObservedEntry> {
    use std::os::unix::fs::MetadataExt;
    let metadata = fs::symlink_metadata(path)?;
    let kind = if metadata.is_dir() {
        EntryKind::Directory
    } else if metadata.is_file() {
        EntryKind::File
    } else {
        EntryKind::Other
    };
    Ok(ObservedEntry {
        id: EntryId {
            volume: metadata.dev(),
            object: u128::from(metadata.ino()),
        },
        kind,
        logical: metadata.len(),
        physical: metadata
            .blocks()
            .checked_mul(512)
            .ok_or_else(|| invalid("allocation overflow"))?,
    })
}

#[cfg(windows)]
pub fn observe(path: &Path) -> io::Result<ObservedEntry> {
    use std::{
        mem::size_of,
        os::windows::{
            ffi::OsStrExt,
            io::{AsRawHandle, FromRawHandle},
        },
    };

    use windows::{
        core::PCWSTR,
        Win32::{
            Foundation::HANDLE,
            Storage::FileSystem::{
                CreateFileW, FileAttributeTagInfo, FileIdInfo, FileStandardInfo,
                GetFileInformationByHandleEx, FILE_ATTRIBUTE_TAG_INFO, FILE_FLAG_BACKUP_SEMANTICS,
                FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_INFO, FILE_READ_ATTRIBUTES,
                FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_STANDARD_INFO,
                OPEN_EXISTING,
            },
        },
    };
    let wide: Vec<_> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    if wide[..wide.len() - 1].contains(&0) {
        return Err(invalid("NUL in observed path"));
    }
    // SAFETY: terminated path lives through CreateFileW; ownership immediately
    // transfers to File. OPEN_REPARSE_POINT inspects the link itself.
    let raw = unsafe {
        CreateFileW(
            PCWSTR(wide.as_ptr()),
            FILE_READ_ATTRIBUTES.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            None,
        )
    }
    .map_err(win_error)?;
    let file = unsafe { fs::File::from_raw_handle(raw.0) };
    let handle = HANDLE(file.as_raw_handle());
    let mut id = FILE_ID_INFO::default();
    let mut sizes = FILE_STANDARD_INFO::default();
    let mut tags = FILE_ATTRIBUTE_TAG_INFO::default();
    // SAFETY: each writable buffer has exactly the documented struct size and
    // the handle stays live for all queries, keeping identity and sizes bound.
    unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileIdInfo,
            std::ptr::addr_of_mut!(id).cast(),
            size_of::<FILE_ID_INFO>() as u32,
        )
        .map_err(win_error)?;
        GetFileInformationByHandleEx(
            handle,
            FileStandardInfo,
            std::ptr::addr_of_mut!(sizes).cast(),
            size_of::<FILE_STANDARD_INFO>() as u32,
        )
        .map_err(win_error)?;
        GetFileInformationByHandleEx(
            handle,
            FileAttributeTagInfo,
            std::ptr::addr_of_mut!(tags).cast(),
            size_of::<FILE_ATTRIBUTE_TAG_INFO>() as u32,
        )
        .map_err(win_error)?;
    }
    if sizes.EndOfFile < 0 || sizes.AllocationSize < 0 {
        return Err(invalid("negative file size"));
    }
    let kind = if tags.FileAttributes & 0x400 != 0 {
        EntryKind::Other
    } else if sizes.Directory {
        EntryKind::Directory
    } else {
        EntryKind::File
    };
    Ok(ObservedEntry {
        id: EntryId {
            volume: id.VolumeSerialNumber,
            object: u128::from_le_bytes(id.FileId.Identifier),
        },
        kind,
        logical: sizes.EndOfFile as u64,
        physical: sizes.AllocationSize as u64,
    })
}

#[cfg(windows)]
fn win_error(error: windows::core::Error) -> io::Error {
    let code = error.code().0 as u32;
    if code & 0xffff0000 == 0x80070000 {
        io::Error::from_raw_os_error((code & 0xffff) as i32)
    } else {
        io::Error::other(error.to_string())
    }
}

#[cfg(not(any(unix, windows)))]
pub fn observe(_path: &Path) -> io::Result<ObservedEntry> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "native index identities unavailable",
    ))
}
