//! Nonpersistent Linux change queues. Every new instance requires a baseline.
use std::{
    collections::{HashMap, HashSet},
    ffi::{CString, OsString},
    io,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::{
            ffi::{OsStrExt, OsStringExt},
            fs::{MetadataExt, OpenOptionsExt},
        },
    },
    path::Path,
    sync::{
        atomic::{AtomicU64, Ordering},
        OnceLock,
    },
    time::{SystemTime, UNIX_EPOCH},
};

use super::{Batch, Change, JournalCursor, JournalKind, NativeJournal};
use crate::index::v2::{EntryId, LinkKey};

// Two descriptors maximum: bound each poll to 1 MiB total and a finite event count.
const BUFFER_LIMIT: usize = 512 * 1024;
const IN_MASK: u32 = libc::IN_CREATE
    | libc::IN_DELETE
    | libc::IN_MOVED_FROM
    | libc::IN_MOVED_TO
    | libc::IN_MODIFY
    | libc::IN_ATTRIB
    | libc::IN_CLOSE_WRITE
    | libc::IN_DELETE_SELF
    | libc::IN_MOVE_SELF
    | libc::IN_ONLYDIR
    | libc::IN_DONT_FOLLOW;
const FAN_MASK: u64 = 0x0000_0fce | 0x4000_0000; // changes, self changes, FAN_ONDIR
const FAN_REPORT_DFID_NAME: u32 = 0x0000_0c00;
static EPOCH: AtomicU64 = AtomicU64::new(1);
static INCARNATION: OnceLock<u64> = OnceLock::new();

pub(crate) struct LinuxJournal {
    cursor: JournalCursor,
    // The root watch also detects unmount/root moves for filesystem fanotify marks.
    inotify: Inotify,
    fanotify: Option<Fanotify>,
    buffer: Vec<u8>,
}

impl LinuxJournal {
    pub(crate) fn open(
        root: &Path,
        root_id: EntryId,
        _resume: Option<JournalCursor>,
    ) -> io::Result<(Self, bool)> {
        let choice = std::env::var("HYPERDU_INDEX_LINUX_BACKEND").unwrap_or_else(|_| "auto".into());
        Self::open_with_backend(root, root_id, &choice)
    }

    fn open_with_backend(root: &Path, root_id: EntryId, choice: &str) -> io::Result<(Self, bool)> {
        if !matches!(choice, "auto" | "fanotify" | "inotify") {
            return Err(invalid(
                "HYPERDU_INDEX_LINUX_BACKEND must be auto, fanotify, or inotify",
            ));
        }
        check_directory(root, root_id)?;
        let mut inotify = Inotify::open(root_id)?;
        inotify.register(root, root_id)?;
        let fanotify = if choice == "inotify" {
            None
        } else {
            match Fanotify::open(root, root_id) {
                Ok(source) => Some(source),
                Err(error) if choice == "auto" => {
                    eprintln!(
                        "hyperdu index: fanotify unavailable ({error}); using recursive inotify"
                    );
                    None
                }
                Err(error) => return Err(error),
            }
        };
        let kind = if fanotify.is_some() {
            JournalKind::Fanotify
        } else {
            JournalKind::Inotify
        };
        let time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_nanos();
        let incarnation = INCARNATION.get_or_init(|| time as u64 ^ u64::from(std::process::id()));
        let serial = EPOCH
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                value.checked_add(1)
            })
            .map_err(|_| invalid("journal incarnation exhausted"))?;
        let epoch = (u128::from(*incarnation) << 64) | u128::from(serial);
        Ok((
            Self {
                cursor: JournalCursor {
                    kind,
                    volume: root_id.volume,
                    epoch,
                    position: 0,
                },
                inotify,
                fanotify,
                buffer: vec![0; BUFFER_LIMIT],
            },
            false,
        ))
    }
}

impl NativeJournal for LinuxJournal {
    fn cursor(&self) -> JournalCursor {
        self.cursor
    }

    fn register_directory(&mut self, path: &Path, id: EntryId) -> io::Result<()> {
        check_directory(path, id)?;
        if id.volume != self.cursor.volume {
            return Err(invalid("index directory crossed a filesystem"));
        }
        match &mut self.fanotify {
            Some(source) => source.register(path, id),
            None => self.inotify.register(path, id),
        }
    }

    fn unregister_directory(&mut self, id: EntryId) -> io::Result<()> {
        if id == self.inotify.root {
            return Err(invalid("cannot unregister index root"));
        }
        match &mut self.fanotify {
            Some(source) => {
                if let Some(key) = source.reverse.remove(&id) {
                    source.handles.remove(&key);
                }
                Ok(())
            }
            None => self.inotify.unregister(id),
        }
    }

    fn poll(&mut self) -> io::Result<Batch> {
        let from = self.cursor;
        let mut changes = Vec::new();
        let (size, mut caught_up) = read_queue(&self.inotify.fd, &mut self.buffer)?;
        self.inotify.decode(&self.buffer[..size], &mut changes)?;
        if let Some(source) = &self.fanotify {
            changes.retain(|change| matches!(change, Change::Reset(_)));
            let (size, empty) = read_queue(&source.fd, &mut self.buffer)?;
            source.decode(&self.buffer[..size], &mut changes)?;
            caught_up &= empty;
        }
        if !changes.is_empty() {
            self.cursor.position = self
                .cursor
                .position
                .checked_add(1)
                .ok_or_else(|| invalid("journal sequence exhausted"))?;
        }
        Ok(Batch {
            from,
            next: self.cursor,
            changes,
            caught_up,
        })
    }
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
fn c_path(path: &Path) -> io::Result<CString> {
    CString::new(path.as_os_str().as_bytes()).map_err(|_| invalid("NUL in directory path"))
}
fn check_directory(path: &Path, id: EntryId) -> io::Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.dev() != id.volume || u128::from(metadata.ino()) != id.object
    {
        return Err(invalid(
            "directory identity changed before watch registration",
        ));
    }
    Ok(())
}
fn open_directory(path: &Path, id: EntryId) -> io::Result<std::fs::File> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    let metadata = file.metadata()?;
    if metadata.dev() != id.volume || u128::from(metadata.ino()) != id.object {
        return Err(invalid(
            "directory identity changed during watch registration",
        ));
    }
    Ok(file)
}
fn owned_fd(fd: libc::c_int) -> io::Result<OwnedFd> {
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        // SAFETY: the successful syscall returned a new descriptor owned exclusively here.
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }
}
fn read_queue(fd: &OwnedFd, buffer: &mut [u8]) -> io::Result<(usize, bool)> {
    // SAFETY: buffer is valid writable storage; the descriptor remains owned throughout read.
    let size = unsafe { libc::read(fd.as_raw_fd(), buffer.as_mut_ptr().cast(), buffer.len()) };
    if size < 0 {
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::WouldBlock {
            return Ok((0, true));
        }
        return Err(error);
    }
    if size == 0 {
        return Err(invalid("native journal unexpectedly reached EOF"));
    }
    Ok((size as usize, false))
}
fn u32_at(bytes: &[u8], offset: usize) -> io::Result<u32> {
    Ok(u32::from_ne_bytes(
        bytes
            .get(offset..offset + 4)
            .ok_or_else(|| invalid("truncated journal integer"))?
            .try_into()
            .unwrap(),
    ))
}
fn name(bytes: &[u8]) -> io::Result<OsString> {
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .ok_or_else(|| invalid("unterminated journal name"))?;
    let bytes = &bytes[..end];
    if bytes.is_empty() || bytes == b"." || bytes == b".." || bytes.contains(&b'/') {
        return Err(invalid("invalid journal entry name"));
    }
    Ok(OsString::from_vec(bytes.to_vec()))
}

struct Inotify {
    fd: OwnedFd,
    root: EntryId,
    watches: HashMap<i32, EntryId>,
    reverse: HashMap<EntryId, i32>,
    retiring: HashSet<i32>,
}
impl Inotify {
    fn open(root: EntryId) -> io::Result<Self> {
        // SAFETY: inotify_init1 has no pointer arguments and returns a new owned fd.
        let fd = owned_fd(unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) })?;
        Ok(Self {
            fd,
            root,
            watches: HashMap::new(),
            reverse: HashMap::new(),
            retiring: HashSet::new(),
        })
    }
    fn register(&mut self, path: &Path, id: EntryId) -> io::Result<()> {
        let directory = open_directory(path, id)?;
        // Bind the watch to an opened identity; final dot satisfies IN_DONT_FOLLOW.
        let path = c_path(Path::new(&format!(
            "/proc/self/fd/{}/.",
            directory.as_raw_fd()
        )))?;
        // SAFETY: the NUL-terminated path and owned descriptor remain live during this call.
        let wd = unsafe { libc::inotify_add_watch(self.fd.as_raw_fd(), path.as_ptr(), IN_MASK) };
        if wd < 0 {
            return Err(io::Error::last_os_error());
        }
        if self.retiring.contains(&wd)
            || self.watches.get(&wd).is_some_and(|old| *old != id)
            || self.reverse.get(&id).is_some_and(|old| *old != wd)
        {
            return Err(invalid(
                "inotify watch descriptor reused before queue drain",
            ));
        }
        self.watches.insert(wd, id);
        self.reverse.insert(id, wd);
        Ok(())
    }
    fn unregister(&mut self, id: EntryId) -> io::Result<()> {
        if let Some(wd) = self.reverse.remove(&id) {
            self.watches.remove(&wd);
            self.retiring.insert(wd);
            // SAFETY: removing this owned watch only queues IN_IGNORED; the fd remains live.
            if unsafe { libc::inotify_rm_watch(self.fd.as_raw_fd(), wd) } < 0 {
                let error = io::Error::last_os_error();
                // An automatically deleted watch already has IN_IGNORED pending.
                if error.raw_os_error() != Some(libc::EINVAL) {
                    return Err(error);
                }
            }
        }
        Ok(())
    }
    fn decode(&mut self, mut bytes: &[u8], changes: &mut Vec<Change>) -> io::Result<()> {
        while !bytes.is_empty() {
            if bytes.len() < 16 {
                return Err(invalid("truncated inotify header"));
            }
            let wd = u32_at(bytes, 0)? as i32;
            let mask = u32_at(bytes, 4)?;
            let len = u32_at(bytes, 12)? as usize;
            let end = 16usize
                .checked_add(len)
                .filter(|end| *end <= bytes.len())
                .ok_or_else(|| invalid("truncated inotify name"))?;
            let record = &bytes[..end];
            bytes = &bytes[end..];
            if mask & libc::IN_Q_OVERFLOW != 0 {
                changes.push(Change::Reset("inotify queue overflow"));
                continue;
            }
            if mask & libc::IN_UNMOUNT != 0 {
                changes.push(Change::Reset("inotify filesystem unmounted"));
                continue;
            }
            if mask & libc::IN_IGNORED != 0 {
                if !self.retiring.remove(&wd) {
                    changes.push(Change::Reset("unexpected inotify watch loss"));
                }
                continue;
            }
            if self.retiring.contains(&wd) {
                continue;
            }
            let id = *self
                .watches
                .get(&wd)
                .ok_or_else(|| invalid("event for unknown inotify watch"))?;
            if mask & (libc::IN_DELETE_SELF | libc::IN_MOVE_SELF) != 0 && id == self.root {
                changes.push(Change::Reset("index root moved or deleted"));
            }
            if mask & libc::IN_DELETE_SELF != 0 {
                self.watches.remove(&wd);
                self.reverse.remove(&id);
                self.retiring.insert(wd);
            }
            if len != 0 {
                changes.push(Change::Entry {
                    key: LinkKey {
                        parent: id,
                        name: name(&record[16..])?,
                    },
                    subtree: mask & (libc::IN_CREATE | libc::IN_ISDIR)
                        == (libc::IN_CREATE | libc::IN_ISDIR),
                });
            }
        }
        Ok(())
    }
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct HandleKey {
    fsid: [u8; 8],
    kind: i32,
    bytes: Vec<u8>,
}
struct Fanotify {
    fd: OwnedFd,
    root: EntryId,
    handles: HashMap<HandleKey, EntryId>,
    reverse: HashMap<EntryId, HandleKey>,
}
impl Fanotify {
    fn open(root: &Path, id: EntryId) -> io::Result<Self> {
        // FAN_CLASS_NOTIF + DFID_NAME generates metadata only. Filesystem marks require
        // CAP_SYS_ADMIN; the kernel validates that capability and filesystem FID support.
        // SAFETY: scalar flags only, newly allocated descriptor is immediately owned.
        let fd = owned_fd(unsafe {
            libc::fanotify_init(
                libc::FAN_CLOEXEC | libc::FAN_NONBLOCK | FAN_REPORT_DFID_NAME,
                libc::O_RDONLY as u32 | libc::O_CLOEXEC as u32,
            )
        })?;
        let path = c_path(root)?;
        // SAFETY: path points to live NUL-terminated bytes and fd belongs to this source.
        if unsafe {
            libc::fanotify_mark(
                fd.as_raw_fd(),
                0x101,
                FAN_MASK,
                libc::AT_FDCWD,
                path.as_ptr(),
            )
        } < 0
        {
            return Err(io::Error::last_os_error());
        }
        let mut source = Self {
            fd,
            root: id,
            handles: HashMap::new(),
            reverse: HashMap::new(),
        };
        source.register(root, id)?;
        Ok(source)
    }
    fn register(&mut self, path: &Path, id: EntryId) -> io::Result<()> {
        let key = directory_handle(path, id)?;
        if self.handles.get(&key).is_some_and(|old| *old != id) {
            return Err(invalid("fanotify directory handle reused"));
        }
        if self.reverse.get(&id).is_some_and(|old| *old != key) {
            return Err(invalid("fanotify identity changed file handle"));
        }
        self.reverse.insert(id, key.clone());
        self.handles.insert(key, id);
        Ok(())
    }
    fn decode(&self, mut bytes: &[u8], changes: &mut Vec<Change>) -> io::Result<()> {
        close_event_descriptors(bytes)?;
        while !bytes.is_empty() {
            if bytes.len() < 24 {
                return Err(invalid("truncated fanotify metadata"));
            }
            let len = u32_at(bytes, 0)? as usize;
            let meta_len = u16::from_ne_bytes([bytes[6], bytes[7]]) as usize;
            let fd = u32_at(bytes, 16)? as i32;

            if bytes[4] != 3 || len < 24 || len > bytes.len() || meta_len < 24 || meta_len > len {
                return Err(invalid("invalid fanotify metadata framing"));
            }
            let mask = u64::from_ne_bytes(bytes[8..16].try_into().unwrap());
            let mut info = &bytes[meta_len..len];
            bytes = &bytes[len..];
            if fd != -1 {
                return Err(invalid("unexpected fanotify event descriptor"));
            }
            if mask & 0x4000 != 0 {
                changes.push(Change::Reset("fanotify queue overflow"));
                continue;
            }
            let mut found = false;
            while !info.is_empty() {
                if info.len() < 4 {
                    return Err(invalid("truncated fanotify info header"));
                }
                let info_len = u16::from_ne_bytes([info[2], info[3]]) as usize;
                if info_len < 20 || info_len > info.len() || !matches!(info[0], 1..=3) {
                    return Err(invalid("unsupported fanotify FID information"));
                }
                let record = &info[..info_len];
                info = &info[info_len..];
                let handle_len = u32_at(record, 12)? as usize;
                let handle_end = 20usize
                    .checked_add(handle_len)
                    .filter(|end| *end <= record.len())
                    .ok_or_else(|| invalid("truncated fanotify handle"))?;
                if handle_len == 0 {
                    return Err(invalid("fanotify missing file handle"));
                }
                let key = HandleKey {
                    fsid: record[4..12].try_into().unwrap(),
                    kind: u32_at(record, 16)? as i32,
                    bytes: record[20..handle_end].to_vec(),
                };
                let parent = self.handles.get(&key);
                if record[0] != 2 && mask & 0x4000_0000 == 0 {
                    return Err(invalid("fanotify file event missing parent name"));
                }
                if record[0] == 2 {
                    // Linux reports a directory itself using DFID_NAME and the name dot.
                    // It is not a child namespace entry and contributes no file totals.
                    if record[handle_end..].starts_with(b".\0") && mask & 0x4000_0000 != 0 {
                        if parent == Some(&self.root) && mask & 0xc00 != 0 {
                            changes.push(Change::Reset("fanotify index root moved or deleted"));
                        }
                        found = true;
                        continue;
                    }
                    let entry_name = name(&record[handle_end..])?;
                    if let Some(parent) = parent {
                        changes.push(Change::Entry {
                            key: LinkKey {
                                parent: *parent,
                                name: entry_name,
                            },
                            // Creation must re-observe children if an inode was reused.
                            subtree: mask & 0x4000_0100 == 0x4000_0100,
                        });
                    }
                    found = true;
                } else if let Some(id) = parent {
                    if mask & 0xc00 != 0 && *id == self.root {
                        changes.push(Change::Reset("fanotify index root moved or deleted"));
                    }
                    // A directory's own metadata is not part of regular-file totals.
                    found = true;
                } else {
                    // The filesystem mark covers directories outside the registered tree.
                    found = true;
                }
            }
            if !found {
                return Err(invalid("fanotify event has no FID information"));
            }
        }
        Ok(())
    }
}

// Dispose of all structurally reachable event descriptors before interpretation.
// Thus rejecting an early event cannot leak descriptors in later events in the read.
fn close_event_descriptors(mut bytes: &[u8]) -> io::Result<()> {
    while !bytes.is_empty() {
        if bytes.len() < 24 {
            return Err(invalid("truncated fanotify metadata"));
        }
        let fd = u32_at(bytes, 16)? as i32;
        if fd >= 0 {
            drop(owned_fd(fd)?);
        }
        let len = u32_at(bytes, 0)? as usize;
        if len < 24 || len > bytes.len() {
            return Err(invalid("invalid fanotify event length"));
        }
        bytes = &bytes[len..];
    }
    Ok(())
}

fn directory_handle(path: &Path, id: EntryId) -> io::Result<HandleKey> {
    #[repr(C)]
    struct FileHandle {
        size: u32,
        kind: i32,
        data: [u8; 128],
    }
    let directory = open_directory(path, id)?;
    let path = c_path(Path::new(""))?;
    let mut handle = FileHandle {
        size: 128,
        kind: 0,
        data: [0; 128],
    };
    let mut mount_id: libc::c_int = 0;
    // SAFETY: FileHandle matches the kernel file_handle header and supplies 128 writable
    // payload bytes. name_to_handle_at writes at most that capacity or returns EOVERFLOW.
    let status = unsafe {
        libc::syscall(
            libc::SYS_name_to_handle_at,
            directory.as_raw_fd(),
            path.as_ptr(),
            &mut handle as *mut FileHandle,
            &mut mount_id,
            libc::AT_EMPTY_PATH,
        )
    };
    if status < 0 {
        return Err(io::Error::last_os_error());
    }
    let len = handle.size as usize;
    if len == 0 || len > handle.data.len() {
        return Err(invalid("unsupported filesystem handle size"));
    }
    let mut stat = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: statfs fills the entire output struct on success.
    if unsafe { libc::fstatfs(directory.as_raw_fd(), stat.as_mut_ptr()) } < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful statfs initialized this value. Linux fsid_t is two native i32s;
    // copy its opaque bytes without depending on libc's private field names.
    let stat = unsafe { stat.assume_init() };
    let mut fsid = [0; 8];
    // SAFETY: the Linux ABI fsid_t occupies eight bytes and fsid is equally sized storage.
    unsafe {
        std::ptr::copy_nonoverlapping(
            (&stat.f_fsid as *const libc::fsid_t).cast::<u8>(),
            fsid.as_mut_ptr(),
            fsid.len(),
        );
    }
    Ok(HandleKey {
        fsid,
        kind: handle.kind,
        bytes: handle.data[..len].to_vec(),
    })
}

#[cfg(test)]
#[path = "linux/tests.rs"]
mod tests;
