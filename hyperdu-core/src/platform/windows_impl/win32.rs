//! `FindFirstFileExW` backend: portable fallback (MinGW builds, or file
//! systems where `NtQueryDirectoryFile` is unusable).
//!
//! `WIN32_FIND_DATAW` carries name, attributes, reparse tag (`dwReserved0`)
//! and logical size. Physical size and file id are not included, so with
//! `compute_physical` or hardlink dedupe enabled this path performs one extra
//! path-based call per file.

use std::sync::atomic::Ordering;

use windows::{
    core::PCWSTR,
    Win32::Storage::FileSystem::{
        FindClose, FindExInfoBasic, FindExSearchNameMatch, FindFirstFileExW, FindNextFileW,
        FIND_FIRST_EX_LARGE_FETCH, WIN32_FIND_DATAW,
    },
};

use super::{
    entry::{claim_visited, file_id_by_path, handle_entry, DirState, RawEntry},
    path::to_wide_for_open,
};
use crate::{
    error_handling::{last_os_error_systemcall, record_error},
    DirContext, ScanContext, StatMap,
};

const STAR: u16 = b'*' as u16;
const SEP: u16 = b'\\' as u16;

pub(super) fn process_dir(ctx: &ScanContext, dctx: &DirContext, map: &mut StatMap) {
    let opt = ctx.options;
    let dir = dctx.dir;
    // <open form>\*\0
    let mut pattern = to_wide_for_open(dir);
    pattern.pop();
    if pattern.last() != Some(&SEP) {
        pattern.push(SEP);
    }
    pattern.push(STAR);
    pattern.push(0);

    let stat_cur = map.entry(dir.to_path_buf()).or_default();
    let mut data: WIN32_FIND_DATAW = unsafe { std::mem::zeroed() };
    let handle = match unsafe {
        FindFirstFileExW(
            PCWSTR(pattern.as_ptr()),
            FindExInfoBasic,
            &mut data as *mut _ as *mut _,
            FindExSearchNameMatch,
            None,
            FIND_FIRST_EX_LARGE_FETCH,
        )
    } {
        Ok(h) => h,
        Err(_) => {
            record_error(opt, &last_os_error_systemcall(dir, "FindFirstFileExW"));
            return;
        }
    };
    let mut st = DirState::new(dir, opt);
    if st.needs_identity(opt) {
        // Rebuild the directory's own open form rather than trimming the search
        // pattern: at a drive or share root there is no separator to trim.
        let identity = file_id_by_path(&to_wide_for_open(dir));
        if let Some((vol, _)) = identity {
            st.volume = vol;
        }
        if !claim_visited(opt, identity) {
            // Already scanned through another path: a link cycle. Stop here.
            let _ = unsafe { FindClose(handle) };
            return;
        }
    }

    loop {
        if opt.cancel.load(Ordering::Relaxed) {
            break;
        }
        let name_len = data
            .cFileName
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(data.cFileName.len());
        let e = RawEntry {
            name: &data.cFileName[..name_len],
            attrs: data.dwFileAttributes,
            reparse_tag: data.dwReserved0,
            logical: ((data.nFileSizeHigh as u64) << 32) | (data.nFileSizeLow as u64),
            alloc: None,
            file_id: None,
        };
        handle_entry(ctx, dctx.depth, &mut st, stat_cur, &e);
        if unsafe { FindNextFileW(handle, &mut data) }.is_err() {
            break;
        }
    }
    let _ = unsafe { FindClose(handle) };
    st.flush_progress(ctx, dir);
}
