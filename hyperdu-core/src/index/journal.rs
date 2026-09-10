//! Native journal positions. A position is useful only with its incarnation
//! and volume identity; loading it does not prove uninterrupted observation.

#[cfg(any(windows, test))]
mod windows_records;

#[cfg(windows)]
pub(crate) mod windows;

/// Native source used for an incremental snapshot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JournalKind {
    Usn,
    Fsevents,
    Fanotify,
    Inotify,
}

/// Last fully applied batch, persisted atomically with its totals.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JournalCursor {
    pub kind: JournalKind,
    pub volume: u64,
    pub epoch: u128,
    pub position: u64,
}
// Native events are hints to re-observe a name, never replacement size values.
use std::{
    io,
    path::{Path, PathBuf},
};

use super::v2::{EntryId, LinkKey};

pub(crate) enum Change {
    #[cfg_attr(not(any(windows, target_os = "linux")), allow(dead_code))]
    Entry { key: LinkKey, subtree: bool },
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    RelativePath { path: PathBuf, subtree: bool },
    #[cfg_attr(not(windows), allow(dead_code))]
    ObjectLinks(EntryId),
    #[cfg_attr(windows, allow(dead_code))]
    Reset(&'static str),
}

pub(crate) struct Batch {
    pub from: JournalCursor,
    pub next: JournalCursor,
    pub changes: Vec<Change>,
    pub caught_up: bool,
}

pub(crate) trait NativeJournal {
    fn cursor(&self) -> JournalCursor;
    fn poll(&mut self) -> io::Result<Batch>;
    /// Inotify must register before a directory's baseline enumeration.
    fn register_directory(&mut self, _path: &Path, _id: EntryId) -> io::Result<()> {
        Ok(())
    }
    /// Release watches after a directory leaves the indexed subtree.
    fn unregister_directory(&mut self, _id: EntryId) -> io::Result<()> {
        Ok(())
    }
}
#[cfg(target_os = "linux")]
pub(crate) mod linux;

#[cfg(target_os = "macos")]
pub(crate) mod macos;

pub(crate) fn open(
    root: &Path,
    root_id: EntryId,
    resume: Option<JournalCursor>,
) -> io::Result<(Box<dyn NativeJournal>, bool)> {
    #[cfg(windows)]
    let (source, resumed) = windows::WindowsJournal::open(root, root_id, resume)?;
    #[cfg(target_os = "linux")]
    let (source, resumed) = linux::LinuxJournal::open(root, root_id, resume)?;
    #[cfg(target_os = "macos")]
    let (source, resumed) = macos::MacJournal::open(root, root_id, resume)?;
    #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
    return Ok((Box::new(source), resumed));
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        let _ = (root, root_id, resume);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "native journal unavailable on this platform",
        ))
    }
}
