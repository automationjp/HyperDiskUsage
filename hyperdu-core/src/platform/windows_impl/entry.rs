//! Per-entry logic shared by the NT and Win32 enumeration backends.

use std::path::{Path, PathBuf};

use windows::{
    core::PCWSTR,
    Win32::{
        Foundation::{CloseHandle, HANDLE},
        Storage::FileSystem::{
            CreateFileW, GetCompressedFileSizeW, GetFileInformationByHandle,
            BY_HANDLE_FILE_INFORMATION, FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_DELETE,
            FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
        },
    },
};

use super::path::ChildPathBuilder;
use crate::{
    common_ops::{check_hardlink_duplicate, update_file_stats},
    path_excluded, wname_matches, Options, ScanContext, Stat,
};

pub(super) const FILE_ATTRIBUTE_DIRECTORY_BIT: u32 = 0x10;
pub(super) const FILE_ATTRIBUTE_REPARSE_POINT_BIT: u32 = 0x400;
pub(super) const IO_REPARSE_TAG_MOUNT_POINT: u32 = 0xA000_0003;
pub(super) const IO_REPARSE_TAG_SYMLINK: u32 = 0xA000_000C;
pub(super) const FILE_READ_ATTRIBUTES: u32 = 0x80;

/// A directory entry as reported by either enumeration API.
pub(super) struct RawEntry<'n> {
    pub name: &'n [u16],
    pub attrs: u32,
    /// Reparse tag when `attrs` has the reparse-point bit (0 otherwise).
    pub reparse_tag: u32,
    pub logical: u64,
    /// Allocation size when the enumeration API provides it.
    pub alloc: Option<u64>,
    /// 64-bit file id when the enumeration API provides it.
    pub file_id: Option<u64>,
}

/// State for one directory being enumerated.
pub(super) struct DirState {
    pub paths: ChildPathBuilder,
    /// Files accounted so far (flushed to the shared counter once per directory).
    pub files: u64,
    /// Volume serial of the directory (set by the backend when `needs_identity`).
    pub volume: u64,
    pub dedupe: bool,
    /// The most recently accounted file, kept only when a progress sample
    /// callback is installed, so the flush can report a real file with the
    /// sizes already read rather than stat'ing it a second time.
    sample: Option<SampleSlot>,
}

impl DirState {
    pub fn new(dir: &Path, opt: &Options) -> Self {
        Self {
            paths: ChildPathBuilder::new(dir),
            files: 0,
            volume: 0,
            dedupe: !opt.count_hardlinks && opt.inode_cache.is_some(),
            sample: opt
                .progress_sample_callback
                .as_ref()
                .map(|_| SampleSlot::default()),
        }
    }

    /// Whether the backend must resolve the directory's own identity
    /// (volume serial, plus file id when cycle detection is active).
    pub fn needs_identity(&self, opt: &Options) -> bool {
        self.dedupe || opt.one_file_system || crate::follows_links(opt)
    }

    /// Hand the directory's file tally to the shared progress counter. The
    /// sample path is only built if a callback actually fires.
    pub fn flush_progress(&mut self, ctx: &ScanContext, dir: &Path) {
        let files = self.files;
        self.files = 0;
        let sample = self.sample.take();
        ctx.report_progress_batch(ctx.options, files, || match &sample {
            Some(s) if !s.name.is_empty() => (self.paths.path(&s.name), s.logical, s.physical),
            _ => (dir.to_path_buf(), 0, 0),
        });
        self.sample = sample.map(|mut s| {
            s.name.clear();
            s
        });
    }
}

/// The last file accounted in a directory, reported when progress fires.
#[derive(Default)]
struct SampleSlot {
    name: Vec<u16>,
    logical: u64,
    physical: u64,
}

#[inline(always)]
fn is_dot_or_dotdot(name: &[u16]) -> bool {
    const DOT: u16 = b'.' as u16;
    matches!(name, [DOT] | [DOT, DOT])
}

#[inline(always)]
fn is_link(attrs: u32, tag: u32) -> bool {
    attrs & FILE_ATTRIBUTE_REPARSE_POINT_BIT != 0
        && (tag == IO_REPARSE_TAG_SYMLINK || tag == IO_REPARSE_TAG_MOUNT_POINT)
}

/// Process one entry: apply filters, enqueue directories, account files.
#[inline]
pub(super) fn handle_entry(
    ctx: &ScanContext,
    depth: u32,
    st: &mut DirState,
    stat_cur: &mut Stat,
    e: &RawEntry,
) {
    let opt = ctx.options;
    if is_dot_or_dotdot(e.name) {
        return;
    }
    let link = is_link(e.attrs, e.reparse_tag);
    if link && !opt.follow_links {
        return;
    }
    if wname_matches(e.name, opt) {
        return;
    }
    let mut child: Option<PathBuf> = None;
    if opt.needs_path_filter {
        let c = st.paths.path(e.name);
        if path_excluded(&c, opt) {
            return;
        }
        child = Some(c);
    }
    if e.attrs & FILE_ATTRIBUTE_DIRECTORY_BIT != 0 {
        handle_dir(ctx, depth, st, e, link, child);
    } else {
        handle_file(opt, st, stat_cur, e);
    }
}

fn handle_dir(
    ctx: &ScanContext,
    depth: u32,
    st: &mut DirState,
    e: &RawEntry,
    link: bool,
    child: Option<PathBuf>,
) {
    let opt = ctx.options;
    if opt.max_depth != 0 && depth >= opt.max_depth {
        return;
    }
    // Only a followed link can leave the volume, and resolving it costs a handle
    // open, so check just those. Cycles are caught when the target is opened for
    // enumeration and registers its own id (see `visited_before`).
    //
    // The comparison is against the scan root, not the parent: `-x` means "stay
    // on the filesystem of the starting point", so a link that lands back on the
    // root's volume is followed even from a directory that is itself elsewhere.
    if link && opt.one_file_system {
        let Some((vol, _)) = file_id_by_path(st.paths.wide_open(e.name)) else {
            return;
        };
        if vol != opt.root_fs_id {
            return;
        }
    }
    let child = child.unwrap_or_else(|| st.paths.path(e.name));
    ctx.enqueue_dir(child, depth + 1);
}

/// Record `(vol, id)` as visited; returns true if it was already there.
///
/// Only the map decides. A bloom "definitely new" answer cannot, because two
/// workers discovering the same directory at once can both receive it: the one
/// that loses the bloom race would still have to consult the map, so the map
/// has to be the single arbiter. Insert returns the previous value, which makes
/// claiming a directory one atomic step.
pub(super) fn visited_before(opt: &Options, vol: u64, id: u64) -> bool {
    let Some(set) = &opt.visited_dirs else {
        return false;
    };
    set.insert((vol, id), ()).is_some()
}

#[inline]
fn handle_file(opt: &Options, st: &mut DirState, stat_cur: &mut Stat, e: &RawEntry) {
    let logical = e.logical;
    if logical < opt.min_file_size {
        return;
    }
    if st.dedupe {
        let id = match e.file_id {
            Some(id) => Some((st.volume, id)),
            None => file_id_by_path(st.paths.wide_open(e.name)),
        };
        if let Some((vol, id)) = id {
            if check_hardlink_duplicate(opt, vol, id) {
                return;
            }
        }
    }
    let physical = if !opt.compute_physical {
        logical
    } else {
        match e.alloc {
            // Zero is a real answer: sparse files, cloud placeholders and empty
            // files occupy no clusters, and GNU du reports them as zero blocks.
            Some(a) => a,
            None => compressed_size_by_path(st.paths.wide_open(e.name)).unwrap_or(logical),
        }
    };
    update_file_stats(stat_cur, logical, physical);
    st.files += 1;
    if let Some(sample) = st.sample.as_mut() {
        sample.name.clear();
        sample.name.extend_from_slice(e.name);
        sample.logical = logical;
        sample.physical = physical;
    }
}

/// `(volume serial, file index)` of the object at `wide_nul`, following links.
pub(super) fn file_id_by_path(wide_nul: &[u16]) -> Option<(u64, u64)> {
    let h = open_for_attributes(wide_nul)?;
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    let r = unsafe { GetFileInformationByHandle(h, &mut info) };
    let _ = unsafe { CloseHandle(h) };
    r.ok()?;
    let id = ((info.nFileIndexHigh as u64) << 32) | (info.nFileIndexLow as u64);
    Some((info.dwVolumeSerialNumber as u64, id))
}

/// `(volume serial, file index)` of an already-open object. No extra syscall
/// beyond the one `GetFileInformationByHandle` the caller needs anyway.
#[cfg(target_env = "msvc")]
pub(super) fn identity_of(h: HANDLE) -> Option<(u64, u64)> {
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    unsafe { GetFileInformationByHandle(h, &mut info) }.ok()?;
    let id = ((info.nFileIndexHigh as u64) << 32) | (info.nFileIndexLow as u64);
    Some((info.dwVolumeSerialNumber as u64, id))
}

/// Record the directory's own identity in the visited set. Returns false when
/// it was already scanned through another path, i.e. a link cycle.
///
/// Call this only once the backend has committed to enumerating the directory:
/// claiming an id and then bailing out would make a fallback backend mistake
/// the directory for a cycle and skip it entirely.
pub(super) fn claim_visited(opt: &Options, id: Option<(u64, u64)>) -> bool {
    if !crate::follows_links(opt) {
        return true;
    }
    match id {
        Some((vol, ino)) => !visited_before(opt, vol, ino),
        None => true,
    }
}

fn open_for_attributes(wide_nul: &[u16]) -> Option<HANDLE> {
    unsafe {
        CreateFileW(
            PCWSTR(wide_nul.as_ptr()),
            FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            None,
        )
    }
    .ok()
}

/// Physical size via `GetCompressedFileSizeW` (Win32 fallback only).
fn compressed_size_by_path(wide_nul: &[u16]) -> Option<u64> {
    let mut high: u32 = 0;
    let low = unsafe { GetCompressedFileSizeW(PCWSTR(wide_nul.as_ptr()), Some(&mut high)) };
    if low == u32::MAX && std::io::Error::last_os_error().raw_os_error() != Some(0) {
        return None;
    }
    Some(((high as u64) << 32) | (low as u64))
}

/// Report a failed NT call. An NTSTATUS is not an errno and must not be passed
/// off as one, so it is reported as a formatted message instead.
pub(super) fn report_nt_status(opt: &Options, dir: &Path, call: &str, status: i32) {
    crate::error_handling::report_error(
        opt,
        dir,
        &format!("{call} failed (NTSTATUS {:#010X})", status as u32),
    );
}
