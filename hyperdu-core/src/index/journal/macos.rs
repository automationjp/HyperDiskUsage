//! Per-device FSEvents with a retained callback context and a serial dispatch
//! queue. References: Apple's FSEvents Programming Guide and FSEvents.h.
use std::{
    ffi::{c_char, c_void, CStr, CString, OsStr},
    io,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    ptr,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use super::{Batch, Change, JournalCursor, JournalKind, NativeJournal};
use crate::index::v2::{observe, EntryId};

const MUST_SCAN: u32 = 0x01;
const DROPPED_OR_WRAPPED: u32 = 0x02 | 0x04 | 0x08;
const HISTORY_DONE: u32 = 0x10;
const ROOT_OR_MOUNT_CHANGED: u32 = 0x20 | 0x40 | 0x80;
const ITEM_CREATED: u32 = 0x100;
const ITEM_RENAMED: u32 = 0x800;
const ITEM_IS_DIR: u32 = 0x20000;
const MAX_EVENTS: usize = 65536;

struct State {
    prefix: PathBuf,
    changes: Vec<Change>,
    position: u64,
    history_done: bool,
    history_end: Option<u64>,
    reset: Option<&'static str>,
}

pub(crate) struct MacJournal {
    stream: *mut c_void,
    queue: *mut c_void,
    array: *const c_void,
    string: *const c_void,
    state: Arc<Mutex<State>>,
    cursor: JournalCursor,
    device: libc::dev_t,
    uuid: Option<u128>,
    started: bool,
}

impl MacJournal {
    pub(crate) fn open(
        root: &Path,
        root_id: EntryId,
        resume: Option<JournalCursor>,
    ) -> io::Result<(Self, bool)> {
        let device = libc::dev_t::try_from(root_id.volume)
            .map_err(|_| invalid("invalid macOS device ID"))?;
        let (mount, kind) = mount_info(root)?;
        if kind != b"apfs" && kind != b"hfs" {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "FSEvents monitoring requires a local APFS or HFS volume",
            ));
        }
        let relative = root
            .strip_prefix(&mount)
            .or_else(|_| root.strip_prefix("/"))
            .map_err(|_| invalid("cannot map root to its filesystem"))?;
        // APFS Data firmlinks may hide the actual mount prefix. Verify the
        // inferred device-relative path by full identity before watching it.
        if observe(&mount.join(relative))?.id != root_id {
            return Err(invalid("device-relative root identity mismatch"));
        }
        let prefix = relative.to_path_buf();
        let uuid = device_uuid(device)?;
        let latest = device_latest(device)?;
        // Apple's sinceWhen contract accepts a saved callback ID directly. The
        // conservative time lookup can trail it, so it only seeds a new scan.
        // HistoryDone below validates the actual replay against this position.
        let resumed = resume.is_some_and(|cursor| can_resume(cursor, uuid, root_id.volume));
        let position = if resumed {
            resume.unwrap().position
        } else {
            latest
        };
        let epoch = uuid.unwrap_or_else(|| {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |time| time.as_nanos())
        });
        let since = if uuid.is_some() { position } else { u64::MAX };
        let state = Arc::new(Mutex::new(State {
            prefix: prefix.clone(),
            changes: Vec::new(),
            position,
            history_done: uuid.is_none(),
            history_end: None,
            reset: None,
        }));
        let name = CString::new(prefix.as_os_str().as_bytes())
            .map_err(|_| invalid("NUL in watched root"))?;
        // SAFETY: terminated filesystem path and immutable objects remain
        // retained in this owner until the stream is invalidated and released.
        let string =
            unsafe { CFStringCreateWithFileSystemRepresentation(ptr::null(), name.as_ptr()) };
        if string.is_null() {
            return Err(invalid("cannot encode FSEvents root"));
        }
        let values = [string];
        let array = unsafe {
            CFArrayCreate(
                ptr::null(),
                values.as_ptr(),
                1,
                ptr::addr_of!(kCFTypeArrayCallBacks).cast(),
            )
        };
        if array.is_null() {
            unsafe { CFRelease(string) };
            return Err(io::Error::other("cannot allocate FSEvents paths"));
        }
        let context = Context {
            version: 0,
            info: Arc::as_ptr(&state).cast_mut().cast(),
            retain: Some(retain_context),
            release: Some(release_context),
            description: None,
        };
        // FileEvents | WatchRoot | NoDefer. Never ignore our own mutations.
        let stream = unsafe {
            FSEventStreamCreateRelativeToDevice(
                ptr::null(),
                callback,
                &context,
                device,
                array,
                since,
                0.05,
                0x10 | 0x04 | 0x02,
            )
        };
        if stream.is_null() {
            unsafe {
                CFRelease(array);
                CFRelease(string);
            }
            return Err(io::Error::other("cannot create FSEvents stream"));
        }
        let label = b"hyperdu.index.fsevents\0";
        let queue = unsafe { dispatch_queue_create(label.as_ptr().cast(), ptr::null()) };
        if queue.is_null() {
            unsafe {
                FSEventStreamRelease(stream);
                CFRelease(array);
                CFRelease(string);
            }
            return Err(io::Error::other("cannot create FSEvents queue"));
        }
        let mut source = Self {
            stream,
            queue,
            array,
            string,
            state,
            device,
            uuid,
            started: false,
            cursor: JournalCursor {
                kind: JournalKind::Fsevents,
                volume: root_id.volume,
                epoch,
                position,
            },
        };
        // SAFETY: the serial queue is distinct from the polling thread, so
        // FlushSync cannot deadlock while waiting for this stream's callback.
        unsafe { FSEventStreamSetDispatchQueue(stream, queue) };
        if unsafe { FSEventStreamStart(stream) } == 0 {
            return Err(io::Error::other("cannot start FSEvents stream"));
        }
        source.started = true;
        Ok((source, resumed))
    }
}

impl NativeJournal for MacJournal {
    fn cursor(&self) -> JournalCursor {
        self.cursor
    }

    fn poll(&mut self) -> io::Result<Batch> {
        let from = self.cursor;
        if device_uuid(self.device)? != self.uuid {
            return Err(invalid("FSEvents device UUID changed"));
        }
        // The time-based device lookup is a conservative replay lower bound,
        // not an upper bound on IDs already delivered by this live stream.
        // Detect invalidation through the UUID and callback reset flags instead.
        // SAFETY: source owns a started stream on its separate dispatch queue.
        // Apple's contract guarantees delivery of all events preceding this call.
        unsafe { FSEventStreamFlushSync(self.stream) };
        let mut state = self
            .state
            .lock()
            .map_err(|_| invalid("FSEvents callback failed"))?;
        let mut changes = std::mem::take(&mut state.changes);
        if let Some(reason) = state.reset.take() {
            changes.push(Change::Reset(reason));
        }
        let next = JournalCursor {
            position: state.position,
            ..from
        };
        let caught_up = state.history_done;
        self.cursor = next;
        Ok(Batch {
            from,
            next,
            changes,
            caught_up,
        })
    }
}

impl Drop for MacJournal {
    fn drop(&mut self) {
        // SAFETY: invalidate stops further dispatch; release drops the stream's
        // retained Arc context. Our own Arc remains live through this teardown.
        unsafe {
            if self.started {
                FSEventStreamStop(self.stream);
            }
            FSEventStreamInvalidate(self.stream);
            FSEventStreamRelease(self.stream);
            dispatch_release(self.queue);
            CFRelease(self.array);
            CFRelease(self.string);
        }
    }
}

extern "C" fn retain_context(info: *const c_void) -> *const c_void {
    // SAFETY: creation passes Arc::as_ptr; CoreServices balances retain/release.
    unsafe { Arc::<Mutex<State>>::increment_strong_count(info.cast()) };
    info
}

extern "C" fn release_context(info: *const c_void) {
    // SAFETY: exactly one retained strong reference is released by this callback.
    unsafe { Arc::<Mutex<State>>::decrement_strong_count(info.cast()) };
}

extern "C" fn callback(
    _stream: *const c_void,
    info: *mut c_void,
    count: usize,
    paths: *mut c_void,
    flags: *const u32,
    ids: *const u64,
) {
    if info.is_null() {
        return;
    }
    // SAFETY: the stream retains this Arc context for every callback.
    let state = unsafe { &*info.cast::<Mutex<State>>() };
    // No unwind crosses the C callback boundary. A failure requires rebuilding.
    let result = std::panic::catch_unwind(|| {
        let mut state = state.lock().unwrap_or_else(|error| error.into_inner());
        if count > MAX_EVENTS || paths.is_null() || flags.is_null() || ids.is_null() {
            state.reset = Some("invalid or excessive FSEvents callback");
            return;
        }
        for index in 0..count {
            // SAFETY: CoreServices supplies count elements, each path is a
            // terminated filesystem representation valid until callback returns.
            let flag = unsafe { *flags.add(index) };
            let id = unsafe { *ids.add(index) };
            if flag & (DROPPED_OR_WRAPPED | ROOT_OR_MOUNT_CHANGED) != 0 {
                state.reset = Some("FSEvents dropped, wrapped, root or mount changed");
                continue;
            }
            if flag & HISTORY_DONE != 0 {
                state.history_end = Some(id);
                // Compare the received ID before max() can hide a regression.
                // A future/rolled-back checkpoint must never become caught up.
                if id < state.position {
                    state.reset = Some("FSEvents history ended before the requested cursor");
                    state.history_done = false;
                } else {
                    state.position = id;
                    state.history_done = true;
                }
                continue; // only the sentinel path is unspecified
            }
            state.position = state.position.max(id);
            let path = unsafe { *paths.cast::<*const c_char>().add(index) };
            if path.is_null() {
                state.reset = Some("missing FSEvents path");
                continue;
            }
            let bytes = unsafe { CStr::from_ptr(path) }.to_bytes();
            queue_path(&mut state, bytes, flag);
        }
    });
    if result.is_err() {
        let mut state = state.lock().unwrap_or_else(|error| error.into_inner());
        state.reset = Some("FSEvents callback panicked");
    }
}

fn queue_path(state: &mut State, bytes: &[u8], flag: u32) {
    if bytes.len() > 65536 || state.changes.len() >= MAX_EVENTS {
        state.reset = Some("FSEvents queue exceeded its bound");
        return;
    }
    let path = Path::new(OsStr::from_bytes(bytes));
    let relative = if let Ok(relative) = path.strip_prefix(&state.prefix) {
        relative.to_path_buf()
    } else if flag & MUST_SCAN != 0 && state.prefix.starts_with(path) {
        PathBuf::new()
    } else {
        // Includes unexpected normalization/path mapping changes: never silently
        // ignore a notification from a stream scoped to this root.
        state.reset = Some("FSEvents path no longer maps to the watched root");
        return;
    };
    if relative
        .components()
        .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        state.reset = Some("FSEvents path escapes root");
        return;
    }
    state.changes.push(Change::RelativePath {
        path: relative,
        // A directory arriving from outside can reuse a stored inode. FSEvents
        // has no paired move cookie, so both create and rename need enumeration.
        subtree: flag & MUST_SCAN != 0
            || (flag & ITEM_IS_DIR != 0 && flag & (ITEM_CREATED | ITEM_RENAMED) != 0),
    });
}

fn can_resume(cursor: JournalCursor, uuid: Option<u128>, volume: u64) -> bool {
    uuid.is_some_and(|uuid| cursor.epoch == uuid)
        && cursor.kind == JournalKind::Fsevents
        && cursor.volume == volume
        // u64::MAX means SinceNow, not a persistent event ID.
        && cursor.position != u64::MAX
}

fn device_uuid(device: libc::dev_t) -> io::Result<Option<u128>> {
    // SAFETY: Copy returns a retained CFUUID or null; release after copying bytes.
    let uuid = unsafe { FSEventsCopyUUIDForDevice(device) };
    if uuid.is_null() {
        return Ok(None);
    }
    let bytes = unsafe { CFUUIDGetUUIDBytes(uuid) };
    unsafe { CFRelease(uuid) };
    Ok(Some(u128::from_le_bytes(bytes.bytes)))
}

fn device_latest(device: libc::dev_t) -> io::Result<u64> {
    // This function's parameter documentation specifies POSIX seconds since
    // 1970, despite the CFAbsoluteTime type name used by its ABI.
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| invalid("system clock is before the journal epoch"))?
        .as_secs_f64();
    Ok(unsafe { FSEventsGetLastEventIdForDeviceBeforeTime(device, seconds) })
}

fn mount_info(root: &Path) -> io::Result<(PathBuf, Vec<u8>)> {
    let path = CString::new(root.as_os_str().as_bytes()).map_err(|_| invalid("NUL in root"))?;
    let mut stat = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: terminated root and writable statfs output buffer have valid sizes.
    if unsafe { libc::statfs(path.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let stat = unsafe { stat.assume_init() };
    let name = |bytes: &[c_char]| -> io::Result<Vec<u8>> {
        let end = bytes
            .iter()
            .position(|byte| *byte == 0)
            .ok_or_else(|| invalid("unterminated statfs name"))?;
        Ok(bytes[..end].iter().map(|byte| *byte as u8).collect())
    };
    let mount = name(&stat.f_mntonname)?;
    Ok((
        PathBuf::from(OsStr::from_bytes(&mount)),
        name(&stat.f_fstypename)?,
    ))
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[repr(C)]
struct Context {
    version: isize,
    info: *mut c_void,
    retain: Option<extern "C" fn(*const c_void) -> *const c_void>,
    release: Option<extern "C" fn(*const c_void)>,
    description: Option<extern "C" fn(*const c_void) -> *const c_void>,
}
#[repr(C)]
struct UuidBytes {
    bytes: [u8; 16],
}
#[repr(C)]
struct ArrayCallbacks {
    version: isize,
    retain: *const c_void,
    release: *const c_void,
    description: *const c_void,
    equal: *const c_void,
}

#[link(name = "CoreServices", kind = "framework")]
extern "C" {
    fn FSEventStreamCreateRelativeToDevice(
        allocator: *const c_void,
        callback: extern "C" fn(
            *const c_void,
            *mut c_void,
            usize,
            *mut c_void,
            *const u32,
            *const u64,
        ),
        context: *const Context,
        device: libc::dev_t,
        paths: *const c_void,
        since: u64,
        latency: f64,
        flags: u32,
    ) -> *mut c_void;
    fn FSEventStreamSetDispatchQueue(stream: *mut c_void, queue: *mut c_void);
    fn FSEventStreamStart(stream: *mut c_void) -> u8;
    fn FSEventStreamStop(stream: *mut c_void);
    fn FSEventStreamInvalidate(stream: *mut c_void);
    fn FSEventStreamRelease(stream: *mut c_void);
    fn FSEventStreamFlushSync(stream: *mut c_void);
    fn FSEventsCopyUUIDForDevice(device: libc::dev_t) -> *const c_void;
    fn FSEventsGetLastEventIdForDeviceBeforeTime(device: libc::dev_t, seconds: f64) -> u64;
}
#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFTypeArrayCallBacks: ArrayCallbacks;
    fn CFStringCreateWithFileSystemRepresentation(
        allocator: *const c_void,
        path: *const c_char,
    ) -> *const c_void;
    fn CFArrayCreate(
        allocator: *const c_void,
        values: *const *const c_void,
        count: isize,
        callbacks: *const c_void,
    ) -> *const c_void;
    fn CFUUIDGetUUIDBytes(uuid: *const c_void) -> UuidBytes;
    fn CFRelease(object: *const c_void);
}
#[link(name = "System")]
extern "C" {
    fn dispatch_queue_create(label: *const c_char, attributes: *const c_void) -> *mut c_void;
    fn dispatch_release(object: *mut c_void);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_scope_and_coalesced_events_are_explicit() {
        let mut state = State {
            prefix: "root/watch".into(),
            changes: vec![],
            position: 2,
            history_done: false,
            history_end: None,
            reset: None,
        };
        queue_path(&mut state, b"root/watch/file", 0x1000);
        assert!(
            matches!(&state.changes[0], Change::RelativePath { path, subtree: false } if path == Path::new("file"))
        );
        queue_path(&mut state, b"root", MUST_SCAN);
        assert!(
            matches!(&state.changes[1], Change::RelativePath { path, subtree: true } if path.as_os_str().is_empty())
        );
        queue_path(&mut state, b"outside", 0x1000);
        assert!(state.reset.is_some());
    }

    #[test]
    fn volume_root_uses_empty_prefix_and_rejects_absolute_callbacks() {
        let mut state = State {
            prefix: PathBuf::new(),
            changes: vec![],
            position: 0,
            history_done: false,
            history_end: None,
            reset: None,
        };
        queue_path(&mut state, b"Users/file", 0);
        assert!(state.reset.is_none());
        assert!(
            matches!(&state.changes[0], Change::RelativePath { path, .. }
            if path == Path::new("Users/file"))
        );
        queue_path(&mut state, b"/Users/file", 0);
        assert!(state.reset.is_some());
    }
    #[test]
    fn directory_arrival_is_reconciled_even_if_its_inode_was_known() {
        let mut state = State {
            prefix: PathBuf::new(),
            changes: vec![],
            position: 0,
            history_done: false,
            history_end: None,
            reset: None,
        };
        queue_path(&mut state, b"created", 0x20100);
        assert!(matches!(
            state.changes[0],
            Change::RelativePath { subtree: true, .. }
        ));
        queue_path(&mut state, b"renamed", 0x20800);
        assert!(matches!(
            state.changes[1],
            Change::RelativePath { subtree: true, .. }
        ));
        queue_path(&mut state, b"renamed-file", 0x10800);
        assert!(matches!(
            state.changes[2],
            Change::RelativePath { subtree: false, .. }
        ));
    }
    #[test]
    fn callback_history_sentinel_and_dropped_events_never_become_changes() {
        let state = Arc::new(Mutex::new(State {
            prefix: "root".into(),
            changes: vec![],
            position: 20,
            history_done: false,
            history_end: None,
            reset: None,
        }));
        let paths = [ptr::null::<c_char>()];
        callback(
            ptr::null(),
            Arc::as_ptr(&state).cast_mut().cast(),
            1,
            paths.as_ptr().cast_mut().cast(),
            [HISTORY_DONE].as_ptr(),
            [30].as_ptr(),
        );
        assert!(state.lock().unwrap().history_done);
        assert_eq!(state.lock().unwrap().position, 30);
        callback(
            ptr::null(),
            Arc::as_ptr(&state).cast_mut().cast(),
            1,
            paths.as_ptr().cast_mut().cast(),
            [DROPPED_OR_WRAPPED].as_ptr(),
            [0].as_ptr(),
        );
        assert!(state.lock().unwrap().reset.is_some());
        assert!(state.lock().unwrap().changes.is_empty());
    }

    #[test]
    fn resume_requires_matching_identity_and_a_persistent_event_id() {
        let cursor = JournalCursor {
            kind: JournalKind::Fsevents,
            epoch: 7,
            volume: 3,
            position: 20,
        };
        assert!(can_resume(cursor, Some(7), 3));
        assert!(!can_resume(cursor, None, 3));
        assert!(!can_resume(cursor, Some(8), 3));
        assert!(!can_resume(cursor, Some(7), 4));
        assert!(!can_resume(
            JournalCursor {
                kind: JournalKind::Usn,
                ..cursor
            },
            Some(7),
            3
        ));
        assert!(!can_resume(
            JournalCursor {
                position: u64::MAX,
                ..cursor
            },
            Some(7),
            3
        ));
    }

    #[test]
    fn history_completion_validates_the_received_id_before_advancing() {
        for (completed, valid) in [(19, false), (20, true), (21, true)] {
            let state = Arc::new(Mutex::new(State {
                prefix: "root".into(),
                changes: vec![],
                position: 20,
                history_done: false,
                history_end: None,
                reset: None,
            }));
            let paths = [ptr::null::<c_char>()];
            callback(
                ptr::null(),
                Arc::as_ptr(&state).cast_mut().cast(),
                1,
                paths.as_ptr().cast_mut().cast(),
                [HISTORY_DONE].as_ptr(),
                [completed].as_ptr(),
            );
            let state = state.lock().unwrap();
            assert_eq!(state.history_done, valid, "completion ID {completed}");
            assert_eq!(state.reset.is_none(), valid, "completion ID {completed}");
            assert_eq!(state.position, completed.max(20));
            assert_eq!(state.history_end, Some(completed));
            assert!(state.changes.is_empty());
        }
    }
    #[test]
    fn native_fsevents_delivers_changes_and_resumes_device_history() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let root_id = observe(&root).unwrap().id;
        let (mut source, resumed) = MacJournal::open(&root, root_id, None).unwrap();
        assert!(!resumed);
        std::fs::write(root.join("first"), b"abc").unwrap();
        let mut witnessed = false;
        for _ in 0..20 {
            let batch = source.poll().unwrap();
            assert!(!batch
                .changes
                .iter()
                .any(|change| matches!(change, Change::Reset(_))));
            witnessed |= batch.changes.iter().any(|change| {
                matches!(change,
                Change::RelativePath { path, .. } if path == Path::new("first"))
            });
            if witnessed && batch.caught_up {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(
            witnessed,
            "native FSEvents did not deliver the created file"
        );
        // Poll again immediately after delivery: a conservative time lookup may
        // still trail the callback cursor and must not invalidate this stream.
        let delivered = source.cursor();
        for _ in 0..3 {
            let batch = source.poll().unwrap();
            assert!(batch.caught_up);
            assert!(batch.next.position >= delivered.position);
            assert!(!batch
                .changes
                .iter()
                .any(|change| matches!(change, Change::Reset(_))));
        }
        let cursor = source.cursor();
        drop(source);
        // No mutation is needed to prove that a saved callback cursor can resume.
        let (mut source, resumed) = MacJournal::open(&root, root_id, Some(cursor)).unwrap();
        assert!(
            resumed,
            "unchanged restart rejected saved={cursor:?}, source={:?}",
            source.cursor()
        );
        let mut caught_up = false;
        for _ in 0..20 {
            let batch = source.poll().unwrap();
            assert!(
                !batch
                    .changes
                    .iter()
                    .any(|change| matches!(change, Change::Reset(_))),
                "unchanged restart reset: saved={cursor:?}, next={:?}, completion={:?}",
                batch.next,
                source.state.lock().unwrap().history_end
            );
            if batch.caught_up {
                caught_up = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(
            caught_up,
            "unchanged restart never completed: saved={cursor:?}, source={:?}, completion={:?}",
            source.cursor(),
            source.state.lock().unwrap().history_end
        );
        let cursor = source.cursor();
        drop(source);
        std::fs::write(root.join("after-restart"), b"def").unwrap();
        let (mut source, resumed) = MacJournal::open(&root, root_id, Some(cursor)).unwrap();
        assert!(
            resumed,
            "device UUID and saved cursor must permit native replay: saved={cursor:?}, source={:?}",
            source.cursor()
        );
        let mut replayed = false;
        let mut replay_caught_up = false;
        for _ in 0..20 {
            let batch = source.poll().unwrap();
            assert!(
                !batch
                    .changes
                    .iter()
                    .any(|change| matches!(change, Change::Reset(_))),
                "offline replay reset: saved={cursor:?}, next={:?}, completion={:?}",
                batch.next,
                source.state.lock().unwrap().history_end
            );
            replay_caught_up = batch.caught_up;
            replayed |= batch.changes.iter().any(|change| {
                matches!(change,
                Change::RelativePath { path, .. } if path == Path::new("after-restart"))
            });
            if replayed && batch.caught_up {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(
            replayed && replay_caught_up,
            "offline replay incomplete: replayed={replayed}, saved={cursor:?}, next={:?}, completion={:?}",
            source.cursor(), source.state.lock().unwrap().history_end
        );
        // A nonempty directory renamed into the watched root needs recursive
        // reconciliation even if its inode happened to be seen there before.
        let outside = tempfile::tempdir_in(root.parent().unwrap()).unwrap();
        std::fs::create_dir_all(outside.path().join("incoming/nested")).unwrap();
        std::fs::write(outside.path().join("incoming/nested/file"), b"moved").unwrap();
        std::fs::rename(outside.path().join("incoming"), root.join("moved-in")).unwrap();
        let mut arrived = false;
        for _ in 0..20 {
            let batch = source.poll().unwrap();
            assert!(
                !batch
                    .changes
                    .iter()
                    .any(|change| matches!(change, Change::Reset(_))),
                "directory arrival reset: next={:?}",
                batch.next
            );
            arrived |= batch.changes.iter().any(|change| {
                matches!(change, Change::RelativePath { path, subtree: true }
                    if path == Path::new("moved-in") || path.as_os_str().is_empty())
            });
            if arrived && batch.caught_up {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(
            arrived,
            "native FSEvents omitted recursive directory arrival"
        );
    }
}
