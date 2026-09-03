//! `NtQueryDirectoryFile` backend (MSVC targets).
//!
//! Each call fills a thread-local buffer with `FILE_ID_FULL_DIR_INFORMATION`
//! records, which carry name, attributes, reparse tag (in `EaSize`), logical
//! size, allocation size and file id. Physical sizes and hardlink dedupe are
//! therefore free per entry. The `FULL` variant is used rather than `ID_BOTH`
//! because it omits the 8.3 short name, making each record 24 bytes smaller so
//! more entries fit per syscall.
//!
//! Buffer size defaults to 64 KiB (measured best on NTFS; larger buffers gave
//! no gain); override with `HYPERDU_WIN_DIR_BUF_KB`.

use std::{cell::RefCell, sync::atomic::Ordering};

use windows::{
    core::PCWSTR,
    Wdk::Storage::FileSystem::{
        FileIdFullDirectoryInformation, NtQueryDirectoryFile, FILE_ID_FULL_DIR_INFORMATION,
        FILE_INFORMATION_CLASS,
    },
    Win32::{
        Foundation::{CloseHandle, HANDLE},
        Storage::FileSystem::{
            CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_DELETE, FILE_SHARE_READ,
            FILE_SHARE_WRITE, OPEN_EXISTING,
        },
        System::IO::IO_STATUS_BLOCK,
    },
};

use super::{
    entry::{
        claim_visited, handle_entry, identity_of, report_nt_status, DirState, RawEntry,
        FILE_READ_ATTRIBUTES,
    },
    path::to_wide_for_open,
};
use crate::{
    error_handling::{last_os_error_systemcall, record_error},
    DirContext, ScanContext, Stat, StatMap,
};

const DEFAULT_DIR_BUFFER_KB: usize = 64;
const FILE_LIST_DIRECTORY: u32 = 0x0001;
const SYNCHRONIZE: u32 = 0x0010_0000;
const STATUS_NO_MORE_FILES: i32 = 0x8000_0006_u32 as i32;
const STATUS_NO_SUCH_FILE: i32 = 0xC000_000F_u32 as i32;
/// Size of the fixed part of `FILE_ID_FULL_DIR_INFORMATION` (up to `FileName`).
const RECORD_HEADER_BYTES: usize = 80;

/// Enumeration buffer size in u64 words, read from the environment once.
fn buffer_words() -> usize {
    use std::sync::OnceLock;
    static WORDS: OnceLock<usize> = OnceLock::new();
    *WORDS.get_or_init(|| {
        let kb = std::env::var("HYPERDU_WIN_DIR_BUF_KB")
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .filter(|kb| *kb > 0)
            .unwrap_or(DEFAULT_DIR_BUFFER_KB)
            .clamp(4, 4096);
        kb * 1024 / 8
    })
}

thread_local! {
    // u64 storage guarantees the 8-byte alignment the records require.
    static NT_BUF: RefCell<Vec<u64>> = const { RefCell::new(Vec::new()) };
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Outcome {
    /// Directory fully handled (possibly with a recorded error).
    Done,
    /// The API is unusable here and nothing was accounted: caller may fall back.
    Unsupported,
}

pub(super) fn process_dir(ctx: &ScanContext, dctx: &DirContext, map: &mut StatMap) -> Outcome {
    let opt = ctx.options;
    let dir = dctx.dir;
    let wide = to_wide_for_open(dir);
    let handle = match open_dir(&wide) {
        Ok(h) => h,
        Err(_) => {
            map.entry(dir.to_path_buf()).or_default();
            record_error(opt, &last_os_error_systemcall(dir, "CreateFileW"));
            return Outcome::Done;
        }
    };
    let mut st = DirState::new(dir, opt);
    // Resolve the identity now, while the handle is open, but do not register it
    // yet: registration happens once the info class is known to work, so that a
    // fallback to the Win32 backend is not mistaken for a cycle.
    let identity = if st.needs_identity(opt) {
        identity_of(handle)
    } else {
        None
    };
    if let Some((vol, _)) = identity {
        st.volume = vol;
    }
    let stat_cur = map.entry(dir.to_path_buf()).or_default();
    let outcome = NT_BUF.with(|cell| {
        let mut buf = cell.borrow_mut();
        let want = buffer_words();
        if buf.len() < want {
            buf.resize(want, 0);
        }
        enumerate(ctx, dctx, handle, &mut buf, &mut st, stat_cur, identity)
    });
    let _ = unsafe { CloseHandle(handle) };
    if outcome == Outcome::Done {
        st.flush_progress(ctx, dir);
    }
    outcome
}

fn open_dir(wide_nul: &[u16]) -> windows::core::Result<HANDLE> {
    unsafe {
        CreateFileW(
            PCWSTR(wide_nul.as_ptr()),
            FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            None,
        )
    }
}

#[allow(clippy::too_many_arguments)]
fn enumerate(
    ctx: &ScanContext,
    dctx: &DirContext,
    handle: HANDLE,
    buf: &mut [u64],
    st: &mut DirState,
    stat_cur: &mut Stat,
    identity: Option<(u64, u64)>,
) -> Outcome {
    let opt = ctx.options;
    let mut first = true;
    loop {
        if opt.cancel.load(Ordering::Relaxed) {
            return Outcome::Done;
        }
        let mut iosb: IO_STATUS_BLOCK = unsafe { std::mem::zeroed() };
        let status = unsafe {
            NtQueryDirectoryFile(
                handle,
                None,
                None,
                None,
                &mut iosb,
                buf.as_mut_ptr() as *mut _,
                (buf.len() * 8) as u32,
                FILE_INFORMATION_CLASS(FileIdFullDirectoryInformation.0),
                false,
                None,
                false,
            )
        };
        if status.is_err() {
            let code = status.0;
            if code == STATUS_NO_MORE_FILES || code == STATUS_NO_SUCH_FILE {
                return Outcome::Done;
            }
            if first {
                return Outcome::Unsupported;
            }
            report_nt_status(opt, dctx.dir, "NtQueryDirectoryFile", code);
            return Outcome::Done;
        }
        if first {
            // The info class works here, so this backend owns the directory.
            first = false;
            if !claim_visited(opt, identity) {
                return Outcome::Done;
            }
        }
        let filled = (iosb.Information).min(buf.len() * 8);
        unsafe {
            walk_records(
                ctx,
                dctx.depth,
                buf.as_ptr() as *const u8,
                filled,
                st,
                stat_cur,
            )
        };
    }
}

/// Iterate the records the kernel wrote into `[base, base+filled)`.
///
/// # Safety
/// `base` must point to `filled` initialized bytes laid out as consecutive
/// `FILE_ID_FULL_DIR_INFORMATION` records (as produced by the kernel).
unsafe fn walk_records(
    ctx: &ScanContext,
    depth: u32,
    base: *const u8,
    filled: usize,
    st: &mut DirState,
    stat_cur: &mut Stat,
) {
    let mut off = 0usize;
    loop {
        if off + RECORD_HEADER_BYTES > filled {
            return;
        }
        let rec = base.add(off) as *const FILE_ID_FULL_DIR_INFORMATION;
        let next = std::ptr::addr_of!((*rec).NextEntryOffset).read_unaligned() as usize;
        let name_bytes = std::ptr::addr_of!((*rec).FileNameLength).read_unaligned() as usize;
        let name_ptr = std::ptr::addr_of!((*rec).FileName) as *const u16;
        let name_len = name_bytes / 2;
        if off + RECORD_HEADER_BYTES + name_len * 2 > filled {
            return;
        }
        let name = std::slice::from_raw_parts(name_ptr, name_len);
        let e = RawEntry {
            name,
            attrs: std::ptr::addr_of!((*rec).FileAttributes).read_unaligned(),
            reparse_tag: std::ptr::addr_of!((*rec).EaSize).read_unaligned(),
            logical: std::ptr::addr_of!((*rec).EndOfFile).read_unaligned() as u64,
            alloc: Some(std::ptr::addr_of!((*rec).AllocationSize).read_unaligned() as u64),
            file_id: Some(std::ptr::addr_of!((*rec).FileId).read_unaligned() as u64),
        };
        handle_entry(ctx, depth, st, stat_cur, &e);
        if next == 0 {
            return;
        }
        off += next;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_header_matches_struct_layout() {
        let probe: FILE_ID_FULL_DIR_INFORMATION = unsafe { std::mem::zeroed() };
        let base = &probe as *const _ as usize;
        let name = std::ptr::addr_of!(probe.FileName) as usize;
        assert_eq!(name - base, RECORD_HEADER_BYTES);
    }
}
