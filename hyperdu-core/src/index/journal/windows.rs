//! Windows USN journal adapter and bounded hard-link projection.
//!
//! The journal cursor is an absolute USN position tied to the journal ID. A
//! position from an old journal is never silently replayed against a new one.
//! The read side is deliberately synchronous and bounded: the replay service
//! decides when to issue another poll and when to reconcile the tree.

use std::{
    collections::BTreeSet,
    ffi::{c_void, OsStr, OsString},
    io,
    mem::size_of,
    os::windows::{
        ffi::{OsStrExt, OsStringExt},
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::{Component, Path, PathBuf},
};

use windows::{
    core::{PCWSTR, PWSTR},
    Win32::{
        Foundation::{
            ERROR_HANDLE_EOF, ERROR_INVALID_FUNCTION, ERROR_INVALID_PARAMETER, ERROR_MORE_DATA,
            ERROR_NOT_SUPPORTED, GENERIC_READ, HANDLE,
        },
        Storage::FileSystem::{
            CreateFileW, ExtendedFileIdType, FileIdType, FindClose, FindFirstFileNameW,
            FindNextFileNameW, GetFinalPathNameByHandleW, GetVolumeInformationW,
            GetVolumeNameForVolumeMountPointW, GetVolumePathNameW, OpenFileById,
            FILE_ATTRIBUTE_NORMAL, FILE_FLAGS_AND_ATTRIBUTES, FILE_FLAG_BACKUP_SEMANTICS,
            FILE_ID_128, FILE_ID_DESCRIPTOR, FILE_ID_DESCRIPTOR_0, FILE_SHARE_DELETE,
            FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, VOLUME_NAME_DOS,
        },
        System::{
            Ioctl::{
                FSCTL_QUERY_USN_JOURNAL, FSCTL_READ_USN_JOURNAL, READ_USN_JOURNAL_DATA_V1,
                USN_JOURNAL_DATA_V0, USN_JOURNAL_DATA_V2, USN_REASON_HARD_LINK_CHANGE,
            },
            IO::DeviceIoControl,
        },
    },
};

use super::{windows_records, Batch, Change, JournalCursor, JournalKind, NativeJournal};
use crate::index::v2::{EntryId, EntryKind, LinkKey};

const READ_BUFFER_SIZE: usize = 256 * 1024;
const MAX_VOLUME_PATH: usize = 32 * 1024;
const MAX_VOLUME_NAME: usize = 256;
const INITIAL_LINK_NAME: usize = 1024;
const MAX_LINK_NAME: usize = 32 * 1024;
const MAX_LINK_NAMES: usize = 4096;

const BACKSLASH: u16 = b'\\' as u16;
const SLASH: u16 = b'/' as u16;
const COLON: u16 = b':' as u16;

/// The stable portion returned by FSCTL_QUERY_USN_JOURNAL.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct JournalState {
    id: u64,
    first: i64,
    lowest: i64,
    next: i64,
}

impl JournalState {
    fn from_v2(data: USN_JOURNAL_DATA_V2) -> io::Result<Self> {
        let state = Self {
            id: data.UsnJournalID,
            first: data.FirstUsn,
            lowest: data.LowestValidUsn,
            next: data.NextUsn,
        };
        state.validate()?;
        Ok(state)
    }

    fn validate(self) -> io::Result<()> {
        if self.id == 0
            || self.first < 0
            || self.lowest < 0
            || self.next < 0
            || self.lower_bound() > self.next
        {
            return Err(invalid("invalid USN journal bounds"));
        }
        Ok(())
    }

    fn lower_bound(self) -> i64 {
        self.first.max(self.lowest)
    }

    fn accepts(self, cursor: JournalCursor, volume: u64) -> bool {
        if cursor.kind != JournalKind::Usn
            || cursor.volume != volume
            || cursor.epoch != u128::from(self.id)
        {
            return false;
        }
        let Ok(position) = i64::try_from(cursor.position) else {
            return false;
        };
        position >= self.lower_bound() && position <= self.next
    }

    fn cursor_at(self, volume: u64, position: i64) -> io::Result<JournalCursor> {
        if position < 0 || position < self.lower_bound() || position > self.next {
            return Err(invalid("USN cursor is outside the journal"));
        }
        Ok(JournalCursor {
            kind: JournalKind::Usn,
            volume,
            epoch: u128::from(self.id),
            position: position as u64,
        })
    }
}

/// A synchronous, bounded USN reader. The volume handle is read-only and is
/// shared by no other operation, so a failed read cannot advance the cursor.
pub(crate) struct WindowsJournal {
    volume: OwnedHandle,
    volume_number: u64,
    cursor: JournalCursor,
    buffer: Vec<u8>,
}

impl WindowsJournal {
    /// Open the volume's existing USN journal.
    ///
    /// A saved cursor is accepted only when its volume, journal ID, and
    /// position are all still valid. An invalid saved cursor starts at the
    /// current end and reports false, requiring a baseline/reconciliation.
    pub(crate) fn open(
        root: &Path,
        root_id: EntryId,
        resume: Option<JournalCursor>,
    ) -> io::Result<(Self, bool)> {
        let observed = crate::index::v2::observe(root)?;
        if observed.kind != EntryKind::Directory || observed.id != root_id {
            return Err(invalid("USN root identity changed before journal open"));
        }

        let mount = volume_root(root)?;
        let mount_serial = volume_serial_number(&mount)?;
        // FILE_ID_INFO carries a 64-bit serial while GetVolumeInformationW
        // exposes the low 32 bits. The volume GUID and this shared low half
        // bind the opened volume to the root identity.
        if (root_id.volume & u64::from(u32::MAX)) != mount_serial {
            return Err(invalid("USN volume does not match index root"));
        }
        let volume_number = root_id.volume;

        let guid = volume_guid(&mount)?;
        let volume = open_volume_guid(&guid)?;

        let state = query_journal(&volume)?;
        let current = state.cursor_at(volume_number, state.next)?;
        let (cursor, resumed) = match resume {
            Some(saved) if state.accepts(saved, volume_number) => (saved, true),
            _ => (current, false),
        };

        Ok((
            Self {
                volume,
                volume_number,
                cursor,
                buffer: vec![0; READ_BUFFER_SIZE],
            },
            resumed,
        ))
    }

    fn query(&self) -> io::Result<JournalState> {
        query_journal(&self.volume)
    }
}

fn validate_post_read(
    before: JournalState,
    after: JournalState,
    start: i64,
    next: i64,
) -> io::Result<()> {
    if after.id != before.id || after.next < before.next {
        return Err(invalid("USN journal changed during a read"));
    }
    if start < after.lower_bound() {
        return Err(invalid("USN read started before post-read bounds"));
    }
    if next < after.lower_bound() || next > after.next {
        return Err(invalid("USN read cursor is outside post-read bounds"));
    }
    Ok(())
}

impl NativeJournal for WindowsJournal {
    fn cursor(&self) -> JournalCursor {
        self.cursor
    }

    fn poll(&mut self) -> io::Result<Batch> {
        let before = self.query()?;
        let from = self.cursor;
        if !before.accepts(from, self.volume_number) {
            return Err(invalid("USN journal was recreated or discarded the cursor"));
        }
        let start = i64::try_from(from.position)
            .map_err(|_| invalid("USN cursor exceeds signed journal range"))?;
        let request = READ_USN_JOURNAL_DATA_V1 {
            StartUsn: start,
            ReasonMask: u32::MAX,
            ReturnOnlyOnClose: 0,
            Timeout: 0,
            BytesToWaitFor: 0,
            UsnJournalID: before.id,
            MinMajorVersion: 2,
            MaxMajorVersion: 3,
        };
        let mut returned = 0u32;
        // SAFETY: request and the bounded writable buffer remain live for the
        // synchronous call; the volume handle is valid for the whole operation.
        unsafe {
            DeviceIoControl(
                raw_handle(&self.volume),
                FSCTL_READ_USN_JOURNAL,
                Some(std::ptr::addr_of!(request).cast::<c_void>()),
                size_of::<READ_USN_JOURNAL_DATA_V1>() as u32,
                Some(self.buffer.as_mut_ptr().cast::<c_void>()),
                self.buffer.len() as u32,
                Some(std::ptr::addr_of_mut!(returned)),
                None,
            )
        }
        .map_err(win_error)?;

        let bytes = returned as usize;
        if bytes < size_of::<i64>() || bytes > self.buffer.len() {
            return Err(invalid("USN read returned an invalid byte count"));
        }
        let (next, records) = windows_records::parse_batch(&self.buffer[..bytes], start)?;
        if records.is_empty() && next != start {
            return Err(invalid("USN read advanced without records"));
        }
        if before.next > start && next <= start {
            return Err(invalid(
                "USN read made no progress while records were available",
            ));
        }

        // Query after the read. The journal can wrap or be recreated while the
        // IOCTL is in flight; either case invalidates the whole transaction.
        let after = self.query()?;
        validate_post_read(before, after, start, next)?;
        let next_cursor = after.cursor_at(self.volume_number, next)?;
        let caught_up = next >= after.next;

        let mut changes = Vec::with_capacity(records.len());
        for record in records {
            let id = EntryId {
                volume: self.volume_number,
                object: u128::from_le_bytes(record.id),
            };
            let parent = EntryId {
                volume: self.volume_number,
                object: u128::from_le_bytes(record.parent),
            };
            changes.push(Change::Entry {
                key: LinkKey {
                    parent,
                    name: OsString::from_wide(&record.name),
                },
                // A USN record identifies one namespace entry. A subtree
                // refresh would overstate what the journal proves.
                subtree: false,
            });
            if record.reason & USN_REASON_HARD_LINK_CHANGE != 0 {
                changes.push(Change::ObjectLinks(id));
            }
        }

        // Publish the cursor only after every structural and epoch check passed.
        let batch = Batch {
            from,
            next: next_cursor,
            changes,
            caught_up,
        };
        self.cursor = next_cursor;
        Ok(batch)
    }
}

fn query_journal(volume: &OwnedHandle) -> io::Result<JournalState> {
    let mut data = USN_JOURNAL_DATA_V2::default();
    let mut returned = 0u32;
    // SAFETY: data is initialized writable storage of the documented V2 size.
    let result = unsafe {
        DeviceIoControl(
            raw_handle(volume),
            FSCTL_QUERY_USN_JOURNAL,
            None,
            0,
            Some(std::ptr::addr_of_mut!(data).cast::<c_void>()),
            size_of::<USN_JOURNAL_DATA_V2>() as u32,
            Some(std::ptr::addr_of_mut!(returned)),
            None,
        )
    };
    result.map_err(win_error)?;
    if returned < size_of::<USN_JOURNAL_DATA_V0>() as u32
        || returned > size_of::<USN_JOURNAL_DATA_V2>() as u32
    {
        return Err(invalid("USN query returned an unsupported structure size"));
    }
    JournalState::from_v2(data)
}

fn raw_handle(owner: &OwnedHandle) -> HANDLE {
    HANDLE(owner.as_raw_handle())
}

fn terminated_wide(path: &Path) -> io::Result<Vec<u16>> {
    let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    if wide.contains(&0) {
        return Err(invalid("NUL in Windows path"));
    }
    wide.push(0);
    Ok(wide)
}

fn volume_device_path(guid: &Path) -> io::Result<Vec<u16>> {
    let units: Vec<u16> = guid.as_os_str().encode_wide().collect();
    if units.len() < 5
        || units[..4] != [BACKSLASH, BACKSLASH, b'?' as u16, BACKSLASH]
        || units.last() != Some(&BACKSLASH)
    {
        return Err(invalid("invalid volume GUID path"));
    }

    let mut device = vec![BACKSLASH, BACKSLASH, b'.' as u16, BACKSLASH];
    device.extend_from_slice(&units[4..units.len() - 1]);
    device.push(0);
    Ok(device)
}
fn open_volume_guid(guid: &Path) -> io::Result<OwnedHandle> {
    let volume_path = volume_device_path(guid)?;
    // SAFETY: volume_path is NUL-terminated and remains live for the call.
    let raw = unsafe {
        CreateFileW(
            PCWSTR(volume_path.as_ptr()),
            GENERIC_READ.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )
    }
    .map_err(win_error)?;
    // SAFETY: CreateFileW returned a valid unique handle.
    Ok(unsafe { OwnedHandle::from_raw_handle(raw.0) })
}

fn open_file_by_descriptor(
    volume: &OwnedHandle,
    descriptor: &FILE_ID_DESCRIPTOR,
    flags: FILE_FLAGS_AND_ATTRIBUTES,
) -> windows::core::Result<HANDLE> {
    // SAFETY: descriptor and volume remain live for this synchronous call.
    unsafe {
        OpenFileById(
            raw_handle(volume),
            std::ptr::addr_of!(*descriptor),
            GENERIC_READ.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            flags,
        )
    }
}

fn file_id_descriptor(id: EntryId, extended: bool) -> FILE_ID_DESCRIPTOR {
    if extended {
        FILE_ID_DESCRIPTOR {
            dwSize: size_of::<FILE_ID_DESCRIPTOR>() as u32,
            Type: ExtendedFileIdType,
            Anonymous: FILE_ID_DESCRIPTOR_0 {
                ExtendedFileId: FILE_ID_128 {
                    Identifier: id.object.to_le_bytes(),
                },
            },
        }
    } else {
        FILE_ID_DESCRIPTOR {
            dwSize: size_of::<FILE_ID_DESCRIPTOR>() as u32,
            Type: FileIdType,
            Anonymous: FILE_ID_DESCRIPTOR_0 {
                FileId: id.object as i64,
            },
        }
    }
}

fn open_file_by_id_with_flags(
    volume: &OwnedHandle,
    id: EntryId,
    flags: FILE_FLAGS_AND_ATTRIBUTES,
) -> io::Result<OwnedHandle> {
    let extended = file_id_descriptor(id, true);
    let raw = match open_file_by_descriptor(volume, &extended, flags) {
        Ok(raw) => raw,
        Err(error)
            if id.object >> 64 == 0
                && (is_win32(&error, ERROR_INVALID_FUNCTION.0)
                    || is_win32(&error, ERROR_INVALID_PARAMETER.0)
                    || is_win32(&error, ERROR_NOT_SUPPORTED.0)) =>
        {
            let legacy = file_id_descriptor(id, false);
            open_file_by_descriptor(volume, &legacy, flags).map_err(win_error)?
        }
        Err(error) => return Err(win_error(error)),
    };
    // SAFETY: OpenFileById returned a valid unique handle.
    Ok(unsafe { OwnedHandle::from_raw_handle(raw.0) })
}

fn open_file_by_id(volume: &OwnedHandle, id: EntryId) -> io::Result<OwnedHandle> {
    open_file_by_id_with_flags(volume, id, FILE_ATTRIBUTE_NORMAL)
}

fn is_stale_hardlink_lookup(id: EntryId, error: &io::Error) -> bool {
    id.object != 0
        && id.object >> 64 == 0
        && error.raw_os_error() == Some(ERROR_INVALID_PARAMETER.0 as i32)
}

fn open_hardlink_target(
    volume: &OwnedHandle,
    id: EntryId,
    root_id: EntryId,
) -> io::Result<Option<OwnedHandle>> {
    match open_file_by_id(volume, id) {
        Ok(file) => Ok(Some(file)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) if is_stale_hardlink_lookup(id, &error) => {
            // A live root ID confirms that this volume accepts the descriptor
            // shape and OpenFileById operation before treating 87 as stale.
            if open_file_by_id_with_flags(volume, root_id, FILE_FLAG_BACKUP_SEMANTICS).is_ok() {
                Ok(None)
            } else {
                Err(error)
            }
        }
        Err(error) => Err(error),
    }
}

fn final_path_by_handle(file: &OwnedHandle) -> io::Result<PathBuf> {
    let mut buffer = vec![0u16; INITIAL_LINK_NAME];
    loop {
        // SAFETY: buffer remains live for the synchronous call and has bounded size.
        let length =
            unsafe { GetFinalPathNameByHandleW(raw_handle(file), &mut buffer, VOLUME_NAME_DOS) };
        let length = usize::try_from(length).map_err(|_| invalid("final path length overflows"))?;
        if length == 0 {
            return Err(io::Error::last_os_error());
        }
        if length < buffer.len() {
            return Ok(PathBuf::from(OsString::from_wide(&buffer[..length])));
        }
        let next = length
            .checked_add(1)
            .ok_or_else(|| invalid("final path length overflows"))?;
        if next > MAX_VOLUME_PATH {
            return Err(invalid("final path exceeds the bounded limit"));
        }
        buffer.resize(next, 0);
    }
}
fn wide_output(output: &[u16], label: &'static str) -> io::Result<OsString> {
    let end = output
        .iter()
        .position(|unit| *unit == 0)
        .ok_or_else(|| invalid(label))?;
    if end == 0 {
        return Err(invalid(label));
    }
    Ok(OsString::from_wide(&output[..end]))
}

/// Return the mount path used as the volume root for path-relative USN names.
pub(crate) fn volume_root(path: &Path) -> io::Result<PathBuf> {
    let input = terminated_wide(path)?;
    let mut output = vec![0u16; MAX_VOLUME_PATH];
    // SAFETY: input and output are valid for the duration of the API call.
    unsafe { GetVolumePathNameW(PCWSTR(input.as_ptr()), &mut output) }.map_err(win_error)?;
    Ok(PathBuf::from(wide_output(
        &output,
        "GetVolumePathNameW returned no path",
    )?))
}

fn volume_guid(mount: &Path) -> io::Result<PathBuf> {
    let input = terminated_wide(mount)?;
    let mut output = vec![0u16; MAX_VOLUME_NAME];
    // SAFETY: mount is a terminated mount path and output is bounded storage.
    unsafe { GetVolumeNameForVolumeMountPointW(PCWSTR(input.as_ptr()), &mut output) }
        .map_err(win_error)?;
    Ok(PathBuf::from(wide_output(
        &output,
        "GetVolumeNameForVolumeMountPointW returned no name",
    )?))
}

fn volume_serial_number(mount: &Path) -> io::Result<u64> {
    let input = terminated_wide(mount)?;
    let mut serial = 0u32;
    // SAFETY: input is terminated and serial is writable for the call.
    unsafe {
        GetVolumeInformationW(
            PCWSTR(input.as_ptr()),
            None,
            Some(std::ptr::addr_of_mut!(serial)),
            None,
            None,
            None,
        )
    }
    .map_err(win_error)?;
    Ok(u64::from(serial))
}

fn win32_code(error: &windows::core::Error) -> Option<u32> {
    let code = error.code().0 as u32;
    (code & 0xffff_0000 == 0x8007_0000).then_some(code & 0xffff)
}

fn win_error(error: windows::core::Error) -> io::Error {
    if let Some(code) = win32_code(&error) {
        io::Error::from_raw_os_error(code as i32)
    } else {
        io::Error::other(error.to_string())
    }
}

fn is_win32(error: &windows::core::Error, code: u32) -> bool {
    win32_code(error) == Some(code)
}

struct FindNameHandle(HANDLE);

impl Drop for FindNameHandle {
    fn drop(&mut self) {
        // SAFETY: this handle came from FindFirstFileNameW and is closed once.
        let _ = unsafe { FindClose(self.0) };
    }
}

fn grow_link_buffer(buffer: &mut Vec<u16>, required: u32) -> io::Result<()> {
    let required = usize::try_from(required)
        .ok()
        .and_then(|length| length.checked_add(1))
        .ok_or_else(|| invalid("USN hardlink name length overflows"))?;
    if required > MAX_LINK_NAME {
        return Err(invalid("USN hardlink name exceeds the bounded limit"));
    }
    let doubled = buffer.len().saturating_mul(2).min(MAX_LINK_NAME);
    let next = required.max(doubled);
    if next <= buffer.len() {
        return Err(invalid("USN hardlink name buffer did not grow"));
    }
    buffer.resize(next, 0);
    Ok(())
}

fn name_from_buffer(buffer: &[u16], length: u32) -> io::Result<OsString> {
    let length = usize::try_from(length).map_err(|_| invalid("USN hardlink length overflows"))?;
    if length > buffer.len() {
        return Err(invalid("USN hardlink length exceeds its buffer"));
    }
    let length = buffer[..length]
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(length);
    if length == 0 {
        return Err(invalid("USN hardlink name is empty"));
    }
    validate_link_units(&buffer[..length])?;
    Ok(OsString::from_wide(&buffer[..length]))
}

/// Enumerate all names returned by FindFirst/FindNextFileNameW.
///
/// Windows returns names relative to the volume root. The raw UTF-16 units are
/// retained, including unpaired surrogates, and path traversal is rejected
/// before any name is joined to a caller-controlled root.
pub(crate) fn hardlink_names(path: &Path) -> io::Result<Vec<OsString>> {
    let input = terminated_wide(path)?;
    let mut buffer = vec![0u16; INITIAL_LINK_NAME];
    let handle = loop {
        let mut length = buffer.len() as u32;
        // SAFETY: input and buffer remain live for this synchronous call.
        match unsafe {
            FindFirstFileNameW(
                PCWSTR(input.as_ptr()),
                0,
                std::ptr::addr_of_mut!(length),
                PWSTR(buffer.as_mut_ptr()),
            )
        } {
            Ok(handle) => break FindNameHandle(handle),
            Err(error) if is_win32(&error, ERROR_MORE_DATA.0) => {
                grow_link_buffer(&mut buffer, length)?;
            }
            Err(error) => return Err(win_error(error)),
        }
    };

    let mut names = Vec::new();
    let first_length = buffer
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(buffer.len());
    names.push(name_from_buffer(&buffer, first_length as u32)?);
    loop {
        if names.len() >= MAX_LINK_NAMES {
            return Err(invalid("USN hardlink count exceeds the bounded limit"));
        }
        let mut length = buffer.len() as u32;
        // SAFETY: handle and buffer remain valid for the synchronous call.
        match unsafe {
            FindNextFileNameW(
                handle.0,
                std::ptr::addr_of_mut!(length),
                PWSTR(buffer.as_mut_ptr()),
            )
        } {
            Ok(()) => names.push(name_from_buffer(&buffer, length)?),
            Err(error) if is_win32(&error, ERROR_MORE_DATA.0) => {
                grow_link_buffer(&mut buffer, length)?;
            }
            Err(error) if is_win32(&error, ERROR_HANDLE_EOF.0) => break,
            Err(error) => return Err(win_error(error)),
        }
    }
    Ok(names)
}

fn validate_component_units(units: &[u16]) -> io::Result<()> {
    if units.is_empty()
        || units == [b'.' as u16]
        || units == [b'.' as u16, b'.' as u16]
        || units
            .iter()
            .any(|unit| matches!(*unit, 0 | SLASH | COLON | BACKSLASH))
    {
        return Err(invalid("USN hardlink component escapes its root"));
    }
    Ok(())
}

fn validate_link_units(units: &[u16]) -> io::Result<()> {
    if units.is_empty() {
        return Err(invalid("USN hardlink name is empty"));
    }
    let first = usize::from(units[0] == BACKSLASH);
    let mut start = first;
    if start == units.len() || (start != 0 && units[start] == BACKSLASH) {
        return Err(invalid("USN hardlink root path is empty"));
    }
    for end in first..=units.len() {
        if end != units.len() && units[end] != BACKSLASH {
            if units[end] == SLASH || units[end] == COLON || units[end] == 0 {
                return Err(invalid("USN hardlink contains an invalid separator"));
            }
            continue;
        }
        validate_component_units(&units[start..end])?;
        start = end + 1;
        if end != units.len() && start == units.len() {
            return Err(invalid("USN hardlink has a trailing separator"));
        }
    }
    Ok(())
}

fn link_relative(name: &OsStr) -> io::Result<PathBuf> {
    let units: Vec<u16> = name.encode_wide().collect();
    validate_link_units(&units)?;
    let first = usize::from(units[0] == BACKSLASH);
    let mut relative = PathBuf::new();
    let mut start = first;
    for end in first..=units.len() {
        if end == units.len() || units[end] == BACKSLASH {
            relative.push(OsString::from_wide(&units[start..end]));
            start = end + 1;
        }
    }
    Ok(relative)
}

fn ascii_fold(unit: u16) -> u16 {
    if (b'A' as u16..=b'Z' as u16).contains(&unit) {
        unit + (b'a' as u16 - b'A' as u16)
    } else {
        unit
    }
}

fn equal_windows_names(left: &OsStr, right: &OsStr) -> bool {
    let mut left = left.encode_wide();
    let mut right = right.encode_wide();
    loop {
        match (left.next(), right.next()) {
            (None, None) => return true,
            (Some(a), Some(b)) if ascii_fold(a) == ascii_fold(b) => {}
            _ => return false,
        }
    }
}

fn normal_components(path: &Path) -> Vec<OsString> {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(name) => Some(name.to_os_string()),
            Component::Prefix(_) | Component::RootDir | Component::CurDir => None,
            Component::ParentDir => None,
        })
        .collect()
}

fn relative_under(root: &Path, path: &Path) -> Option<PathBuf> {
    let root_components = normal_components(root);
    let path_components = normal_components(path);
    if path_components.len() < root_components.len()
        || !root_components
            .iter()
            .zip(path_components.iter())
            .all(|(left, right)| equal_windows_names(left, right))
    {
        return None;
    }
    let mut relative = PathBuf::new();
    for component in path_components.into_iter().skip(root_components.len()) {
        relative.push(component);
    }
    Some(relative)
}

/// Return all hardlinks for an identity that can be opened directly by the volume.
///
/// OpenFileById supplies a live path to the kernel, so this operation does not
/// scan the indexed tree to rediscover an object. Names are then projected under
/// root; links outside that subtree are ignored.
pub(crate) fn hardlinks_for_id(root: &Path, id: EntryId) -> io::Result<Vec<PathBuf>> {
    let root_entry = crate::index::v2::observe(root)?;
    if root_entry.kind != EntryKind::Directory {
        return Err(invalid("hardlink root is not a directory"));
    }
    if id.volume != root_entry.id.volume {
        return Ok(Vec::new());
    }

    let root_mount = volume_root(root)?;
    let guid = volume_guid(&root_mount)?;
    let volume = open_volume_guid(&guid)?;
    let Some(file) = open_hardlink_target(&volume, id, root_entry.id)? else {
        return Ok(Vec::new());
    };
    let live_path = final_path_by_handle(&file)?;

    let mut links = BTreeSet::new();
    for name in hardlink_names(&live_path)? {
        let relative_to_volume = link_relative(&name)?;
        let absolute = root_mount.join(relative_to_volume);
        if let Some(relative) = relative_under(root, &absolute) {
            if !relative.as_os_str().is_empty() {
                links.insert(relative);
            }
        }
    }
    Ok(links.into_iter().collect())
}
fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use windows::Win32::Storage::FileSystem::{FILE_FLAG_BACKUP_SEMANTICS, FILE_READ_ATTRIBUTES};

    #[test]
    fn journal_cursor_accepts_only_current_incarnation_and_bounds() {
        let state = JournalState {
            id: 9,
            first: 10,
            lowest: 12,
            next: 20,
        };
        let valid = state.cursor_at(7, 12).unwrap();
        assert!(state.accepts(valid, 7));
        assert!(!state.accepts(
            JournalCursor {
                kind: JournalKind::Usn,
                volume: 7,
                epoch: 9,
                position: 11,
            },
            7
        ));
        assert!(!state.accepts(
            JournalCursor {
                kind: JournalKind::Usn,
                volume: 7,
                epoch: 8,
                position: 12,
            },
            7
        ));
        assert!(!state.accepts(
            JournalCursor {
                kind: JournalKind::Usn,
                volume: 6,
                epoch: 9,
                position: 12,
            },
            7
        ));
    }

    #[test]
    fn invalid_journal_bounds_are_rejected() {
        for state in [
            JournalState {
                id: 0,
                first: 0,
                lowest: 0,
                next: 0,
            },
            JournalState {
                id: 1,
                first: -1,
                lowest: 0,
                next: 1,
            },
            JournalState {
                id: 1,
                first: 8,
                lowest: 9,
                next: 8,
            },
        ] {
            assert!(state.validate().is_err());
        }
    }

    #[test]
    fn link_buffer_growth_stays_within_bound() {
        let mut buffer = vec![0; MAX_LINK_NAME / 2 + 1];
        grow_link_buffer(&mut buffer, 0).unwrap();
        assert_eq!(buffer.len(), MAX_LINK_NAME);

        let mut full = vec![0; MAX_LINK_NAME];
        assert!(grow_link_buffer(&mut full, 0).is_err());
        let mut small = vec![0; 1];
        assert!(grow_link_buffer(&mut small, MAX_LINK_NAME as u32).is_err());
    }
    #[test]
    fn volume_device_path_converts_guid_namespace() {
        let guid = PathBuf::from(OsString::from_wide(&[
            BACKSLASH,
            BACKSLASH,
            b'?' as u16,
            BACKSLASH,
            b'V' as u16,
            b'o' as u16,
            b'l' as u16,
            b'u' as u16,
            b'm' as u16,
            b'e' as u16,
            b'{' as u16,
            b'1' as u16,
            b'}' as u16,
            BACKSLASH,
        ]));
        assert_eq!(
            volume_device_path(&guid).unwrap(),
            vec![
                BACKSLASH,
                BACKSLASH,
                b'.' as u16,
                BACKSLASH,
                b'V' as u16,
                b'o' as u16,
                b'l' as u16,
                b'u' as u16,
                b'm' as u16,
                b'e' as u16,
                b'{' as u16,
                b'1' as u16,
                b'}' as u16,
                0,
            ]
        );
        assert!(volume_device_path(Path::new(r"C:\")).is_err());
    }
    #[test]
    fn link_validation_preserves_unpaired_utf16_and_rejects_traversal() {
        assert!(validate_link_units(&[BACKSLASH, b'a' as u16, 0xd800]).is_ok());
        assert!(validate_link_units(&[BACKSLASH, b'.' as u16, b'.' as u16]).is_err());
        assert!(validate_link_units(&[BACKSLASH, b'a' as u16, BACKSLASH]).is_err());
        assert!(validate_link_units(&[BACKSLASH, BACKSLASH, b'a' as u16]).is_err());

        let raw = OsString::from_wide(&[BACKSLASH, b'a' as u16, 0xd800]);
        let relative = link_relative(&raw).unwrap();
        assert_eq!(
            relative.as_os_str().encode_wide().collect::<Vec<_>>(),
            vec![b'a' as u16, 0xd800]
        );
    }

    #[test]
    fn component_comparison_folds_ascii_but_keeps_utf16_units() {
        let lower = OsString::from_wide(&[b'c' as u16, 0xd800]);
        let upper = OsString::from_wide(&[b'C' as u16, 0xd800]);
        assert!(equal_windows_names(&lower, &upper));
        assert!(!equal_windows_names(&lower, &OsString::from("d")));
        assert!(!equal_windows_names(
            &lower,
            &OsString::from_wide(&[b'C' as u16, 0xd801])
        ));
    }

    #[test]
    fn relative_projection_is_case_insensitive_by_component() {
        let root = Path::new(r"C:\Root\Sub");
        let path = Path::new(r"c:\root\sub\Child");
        assert_eq!(relative_under(root, path).unwrap(), PathBuf::from("Child"));
        assert!(relative_under(root, Path::new(r"C:\Root\Other")).is_none());
    }

    #[test]
    fn post_read_validation_rejects_retention_advance_and_epoch_changes() {
        let before = JournalState {
            id: 9,
            first: 10,
            lowest: 10,
            next: 20,
        };
        let first_advanced = JournalState {
            id: 9,
            first: 15,
            lowest: 10,
            next: 25,
        };
        let lowest_advanced = JournalState {
            id: 9,
            first: 10,
            lowest: 15,
            next: 25,
        };
        assert!(validate_post_read(before, first_advanced, 12, 20).is_err());
        assert!(validate_post_read(before, lowest_advanced, 12, 20).is_err());
        assert!(validate_post_read(before, first_advanced, 15, 20).is_ok());
        assert!(validate_post_read(
            before,
            JournalState {
                id: 10,
                first: 10,
                lowest: 10,
                next: 25,
            },
            10,
            20,
        )
        .is_err());
        assert!(validate_post_read(
            before,
            JournalState {
                id: 9,
                first: 10,
                lowest: 10,
                next: 19,
            },
            10,
            18,
        )
        .is_err());
    }
    #[test]
    fn native_hardlink_names_finish_at_eof_and_preserve_real_errors() -> io::Result<()> {
        let fixture = tempfile::tempdir()?;
        let original = fixture.path().join("original");
        let alias = fixture.path().join("alias");
        fs::write(&original, b"hardlink enumeration")?;
        let leaves = |names: Vec<OsString>| {
            names
                .into_iter()
                .map(|name| PathBuf::from(name).file_name().unwrap().to_os_string())
                .collect::<BTreeSet<_>>()
        };
        assert_eq!(
            leaves(hardlink_names(&original)?),
            BTreeSet::from([OsString::from("original")])
        );
        fs::hard_link(&original, &alias)?;
        assert_eq!(
            leaves(hardlink_names(&original)?),
            BTreeSet::from([OsString::from("original"), OsString::from("alias")])
        );
        fs::remove_file(&alias)?;
        assert_eq!(
            leaves(hardlink_names(&original)?),
            BTreeSet::from([OsString::from("original")])
        );
        fs::remove_file(&original)?;
        assert_eq!(
            hardlink_names(&original).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        Ok(())
    }

    fn open_directory_hint(path: &Path) -> io::Result<OwnedHandle> {
        let input = terminated_wide(path)?;
        // SAFETY: input remains live for this synchronous call.
        let raw = unsafe {
            CreateFileW(
                PCWSTR(input.as_ptr()),
                FILE_READ_ATTRIBUTES.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                None,
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS,
                None,
            )
        }
        .map_err(win_error)?;
        // SAFETY: CreateFileW returned a valid unique handle.
        Ok(unsafe { OwnedHandle::from_raw_handle(raw.0) })
    }

    #[test]
    fn stale_hardlink_lookup_requires_64_bit_nonzero_id_and_error_87() {
        let invalid_parameter = io::Error::from_raw_os_error(ERROR_INVALID_PARAMETER.0 as i32);
        let other_error = io::Error::from_raw_os_error(ERROR_HANDLE_EOF.0 as i32);
        assert!(is_stale_hardlink_lookup(
            EntryId {
                volume: 7,
                object: 1,
            },
            &invalid_parameter,
        ));
        assert!(!is_stale_hardlink_lookup(
            EntryId {
                volume: 7,
                object: 0,
            },
            &invalid_parameter,
        ));
        assert!(!is_stale_hardlink_lookup(
            EntryId {
                volume: 7,
                object: (1u128 << 64) | 1,
            },
            &invalid_parameter,
        ));
        assert!(!is_stale_hardlink_lookup(
            EntryId {
                volume: 7,
                object: 1,
            },
            &other_error,
        ));
    }

    #[test]
    fn native_open_file_by_id_deleted_identity_is_empty() -> io::Result<()> {
        let fixture = tempfile::tempdir()?;
        let root = fixture.path();
        let target = root.join("target");
        let alias = root.join("alias");
        fs::write(&target, b"open by id")?;
        fs::hard_link(&target, &alias)?;

        let hint = open_directory_hint(root)?;
        let root_id = crate::index::v2::observe(root)?.id;
        let target_id = crate::index::v2::observe(&target)?.id;
        drop(open_file_by_id_with_flags(
            &hint,
            root_id,
            FILE_FLAG_BACKUP_SEMANTICS,
        )?);

        let live = open_hardlink_target(&hint, target_id, root_id)?;
        assert!(live.is_some(), "live hardlink target did not open");
        drop(live);

        fs::remove_file(&alias)?;
        let remaining = open_hardlink_target(&hint, target_id, root_id)?;
        assert!(remaining.is_some(), "single hardlink target did not open");
        drop(remaining);

        fs::remove_file(&target)?;
        // A failed same-volume support probe must keep the original error.
        let unsupported = open_hardlink_target(&hint, target_id, target_id).unwrap_err();
        assert_eq!(
            unsupported.raw_os_error(),
            Some(ERROR_INVALID_PARAMETER.0 as i32)
        );
        let deleted = match open_hardlink_target(&hint, target_id, root_id) {
            Ok(value) => value,
            Err(error) => panic!(
                "deleted hardlink identity lookup failed with code {:?}: {error}",
                error.raw_os_error()
            ),
        };
        assert!(
            deleted.is_none(),
            "deleted hardlink identity unexpectedly opened"
        );
        Ok(())
    }

    #[test]
    fn native_usn_fixture_is_opt_in() -> io::Result<()> {
        let Some(root) = std::env::var_os("HYPERDU_TEST_USN_ROOT") else {
            eprintln!("NOT RUN: HYPERDU_TEST_USN_ROOT is unset");
            return Ok(());
        };
        let root = PathBuf::from(root);
        let root_id = crate::index::v2::observe(&root)?.id;
        let (mut source, resumed) =
            WindowsJournal::open(&root, root_id, None).expect("USN fixture: open initial journal");
        assert!(!resumed);

        let suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_nanos();
        let fixture = root.join(format!(".hyperdu-usn-{}-{suffix}", std::process::id()));
        fs::create_dir(&fixture)?;
        let fixture_id = crate::index::v2::observe(&fixture)?.id;
        let original = fixture.join("original");
        let renamed = fixture.join("renamed");
        let link = fixture.join("link");
        fs::write(&original, b"usn fixture")?;
        fs::rename(&original, &renamed)?;
        fs::hard_link(&renamed, &link)?;

        let renamed_id = crate::index::v2::observe(&renamed)?.id;
        let link_id = crate::index::v2::observe(&link)?.id;
        assert_eq!(renamed_id, link_id);

        let mut saw_renamed = false;
        let mut saw_link = false;
        let mut saw_hardlink_identity = false;
        for _ in 0..128 {
            let batch = source.poll().expect("USN fixture: poll native journal");
            for change in batch.changes {
                match change {
                    Change::Entry { key, .. } if key.parent == fixture_id => {
                        saw_renamed |= key.name == "renamed";
                        saw_link |= key.name == "link";
                    }
                    Change::ObjectLinks(id) if id == renamed_id => {
                        saw_hardlink_identity = true;
                    }
                    _ => {}
                }
            }
            if saw_renamed && saw_link && saw_hardlink_identity && batch.caught_up {
                break;
            }
        }
        assert!(saw_renamed, "USN fixture did not report renamed name");
        assert!(saw_link, "USN fixture did not report hard-link name");
        assert!(
            saw_hardlink_identity,
            "USN fixture did not report the hard-linked file identity"
        );
        let names =
            hardlinks_for_id(&root, renamed_id).expect("USN fixture: resolve live hardlinks");
        let fixture_name = fixture
            .file_name()
            .ok_or_else(|| invalid("fixture has no name"))?
            .to_os_string();
        assert!(names
            .iter()
            .any(|path| path == &PathBuf::from(&fixture_name).join("renamed")));
        assert!(names
            .iter()
            .any(|path| path == &PathBuf::from(&fixture_name).join("link")));

        let saved = source.cursor();
        let (_, resumed) = WindowsJournal::open(&root, root_id, Some(saved))
            .expect("USN fixture: resume saved journal");
        assert!(resumed);

        fs::remove_file(&link)?;
        fs::remove_file(&renamed)?;
        let mut saw_deleted_renamed = false;
        let mut saw_deleted_link = false;
        for _ in 0..128 {
            let batch = source.poll().expect("USN fixture: poll native journal");
            for change in batch.changes {
                if let Change::Entry { key, .. } = change {
                    if key.parent == fixture_id {
                        saw_deleted_renamed |= key.name == "renamed";
                        saw_deleted_link |= key.name == "link";
                    }
                }
            }
            if saw_deleted_renamed && saw_deleted_link && batch.caught_up {
                break;
            }
        }
        assert!(
            saw_deleted_renamed,
            "USN fixture did not report renamed deletion"
        );
        assert!(
            saw_deleted_link,
            "USN fixture did not report hard-link deletion"
        );
        assert!(hardlinks_for_id(&root, renamed_id)
            .expect("USN fixture: resolve deleted hardlinks")
            .is_empty());
        fs::remove_dir(&fixture)?;
        Ok(())
    }
}
