use std::sync::atomic::Ordering;

use crate::{wname_contains_patterns_lossy, DirContext, ScanContext, StatMap};

pub fn process_dir(ctx: &ScanContext, dctx: &DirContext, map: &mut StatMap) {
    let dir = dctx.dir;
    let depth = dctx.depth;
    let opt = ctx.options;
    // Avoid CreateFileW by default: keep NtQuery fast path behind opt-in env
    if std::env::var("HYPERDU_WIN_USE_NTQUERY").ok().as_deref() == Some("1")
        && try_fast_enum(dir, depth, opt, map, ctx)
    {
        return;
    }
    use std::{
        ffi::OsString,
        os::windows::ffi::{OsStrExt, OsStringExt},
    };

    use windows::{
        core::PCWSTR,
        Win32::{
            Foundation::CloseHandle,
            Storage::FileSystem::{
                CreateFileW, FindClose, FindExInfoBasic, FindExSearchNameMatch, FindFirstFileExW,
                FindNextFileW, GetCompressedFileSizeW, GetFileInformationByHandle,
                BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL,
                FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_DELETE,
                FILE_SHARE_READ, FILE_SHARE_WRITE, FIND_FIRST_EX_LARGE_FETCH, OPEN_EXISTING,
                WIN32_FIND_DATAW,
            },
        },
    };

    // Fast-path: if exclude patterns contain no path separators, we can skip
    // per-file full path construction for many checks and rely on name-only matching.
    let fast_exclude = !opt
        .exclude_contains
        .iter()
        .any(|s| s.as_bytes().iter().any(|&c| c == b'/' || c == b'\\'));

    #[cfg(any(feature = "prof-tracy", feature = "prof-puffin"))]
    profiling::scope!("win32_find_first");
    let mut pattern: Vec<u16> = dir.as_os_str().encode_wide().collect();
    let last = pattern.last().copied();
    if last != Some(92) && last != Some(47) {
        pattern.push(92);
    }
    pattern.push('*' as u16);
    pattern.push(0);

    // Pre-fetch the stats entry for current directory to avoid repeated lookups
    let stat_cur = map.entry(dir.to_path_buf()).or_default();

    // Reusable wide buffer for GetCompressedFileSizeW path building (use \\?\ prefix for long paths)
    let mut base_w: Vec<u16> = dir.as_os_str().encode_wide().collect();
    // Prepend \\?\ if not present and looks like drive path (best-effort)
    if base_w.len() >= 2 && base_w[1] == ':' as u16 {
        let prefix: [u16; 4] = ['\\' as u16, '\\' as u16, '?' as u16, '\\' as u16];
        base_w.splice(0..0, prefix);
    }
    let last2 = base_w.last().copied();
    if last2 != Some(92) && last2 != Some(47) {
        base_w.push(92);
    }
    let base_len = base_w.len();
    let mut wide_buf: Vec<u16> = Vec::with_capacity(base_len + 260);
    wide_buf.extend_from_slice(&base_w);

    unsafe {
        // One-file-system: get current directory's volume serial (avoid CreateFileW unless explicitly allowed)
        let cur_vol_serial: u64 = if opt.one_file_system && opt.win_allow_handle {
            let mut curw: Vec<u16> = dir.as_os_str().encode_wide().collect();
            if curw.last().copied() != Some(0) {
                curw.push(0);
            }
            match {
                unsafe {
                    CreateFileW(
                        PCWSTR(curw.as_ptr()),
                        0x80,
                        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                        None,
                        OPEN_EXISTING,
                        FILE_ATTRIBUTE_NORMAL | FILE_FLAG_BACKUP_SEMANTICS,
                        None,
                    )
                }
            } {
                Ok(h) => {
                    let mut info: BY_HANDLE_FILE_INFORMATION = std::mem::zeroed();
                    let serial =
                        if unsafe { GetFileInformationByHandle(h, &mut info as *mut _ as *mut _) }
                            .is_ok()
                        {
                            info.dwVolumeSerialNumber as u64
                        } else {
                            0
                        };
                    let _ = CloseHandle(h);
                    serial
                }
                Err(_) => 0,
            }
        } else {
            0
        };
        let mut data: WIN32_FIND_DATAW = std::mem::zeroed();
        let handle = match FindFirstFileExW(
            PCWSTR(pattern.as_ptr()),
            FindExInfoBasic,
            &mut data as *mut _ as *mut _,
            FindExSearchNameMatch,
            None,
            FIND_FIRST_EX_LARGE_FETCH,
        ) {
            Ok(h) => h,
            Err(_) => {
                crate::error_handling::record_error(
                    opt,
                    &crate::error_handling::last_os_error_systemcall(dir, "FindFirstFileExW"),
                );
                return;
            }
        };

        let mut first = true;
        loop {
            if !first {
                let ok = FindNextFileW(handle, &mut data).is_ok();
                if !ok {
                    break;
                }
            }
            first = false;

            let name_len = (0..).take_while(|&i| data.cFileName[i] != 0).count();
            let name = OsString::from_wide(&data.cFileName[..name_len]);
            if name == OsString::from(".") || name == OsString::from("..") {
                continue;
            }

            let is_dir = (data.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY.0) != 0;
            let is_reparse = (data.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT.0) != 0;
            if is_reparse && !opt.follow_links {
                continue;
            }

            if wname_contains_patterns_lossy(&name, &opt.exclude_contains) {
                continue;
            }
            if !fast_exclude {
                let child = dir.join(&name);
                if crate::path_excluded(&child, opt) {
                    continue;
                }
            }

            if is_dir {
                if opt.max_depth == 0 || depth < opt.max_depth {
                    let child = dir.join(&name);
                    if opt.one_file_system && cur_vol_serial != 0 {
                        // Check child's volume serial
                        let tfiles = ctx.total_files.load(Ordering::Relaxed);
                        if opt.win_allow_handle
                            && opt.win_handle_sample_every > 0
                            && tfiles % opt.win_handle_sample_every == 0
                        {
                            wide_buf.truncate(base_len);
                            wide_buf.extend_from_slice(&data.cFileName[..name_len]);
                            wide_buf.push(0);
                            if let Ok(h) = CreateFileW(
                                PCWSTR(wide_buf.as_ptr()),
                                0x80,
                                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                                None,
                                OPEN_EXISTING,
                                FILE_ATTRIBUTE_NORMAL | FILE_FLAG_BACKUP_SEMANTICS,
                                None,
                            ) {
                                let mut info: BY_HANDLE_FILE_INFORMATION = std::mem::zeroed();
                                if GetFileInformationByHandle(h, &mut info as *mut _ as *mut _)
                                    .is_ok()
                                {
                                    let vol = info.dwVolumeSerialNumber as u64;
                                    let _ = CloseHandle(h);
                                    if vol != cur_vol_serial {
                                        continue;
                                    }
                                    if opt.follow_links {
                                        if let Some(vset) = &opt.visited_dirs {
                                            let ino = ((info.nFileIndexHigh as u64) << 32)
                                                | (info.nFileIndexLow as u64);
                                            if let Some(bf) = &opt.visited_bloom {
                                                if bf.test_and_set(vol, ino) {
                                                    continue;
                                                }
                                            }
                                            if vset.insert((vol, ino), ()).is_some() {
                                                continue;
                                            }
                                        }
                                    }
                                } else {
                                    let _ = CloseHandle(h);
                                }
                            }
                        }
                    }
                    ctx.enqueue_dir(child, depth + 1);
                }
            } else {
                let logical = ((data.nFileSizeHigh as u64) << 32) | (data.nFileSizeLow as u64);
                if logical >= opt.min_file_size {
                    // Hardlink重複排除（サンプリングしながらハンドル開く）
                    if opt.win_allow_handle
                        && opt.win_handle_sample_every > 0
                        && ctx.total_files.load(Ordering::Relaxed) % opt.win_handle_sample_every
                            == 0
                    {
                        wide_buf.truncate(base_len);
                        wide_buf.extend_from_slice(&data.cFileName[..name_len]);
                        wide_buf.push(0);
                        if let Ok(handle_file) = CreateFileW(
                            PCWSTR(wide_buf.as_ptr()),
                            0x80,
                            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                            None,
                            OPEN_EXISTING,
                            FILE_ATTRIBUTE_NORMAL | FILE_FLAG_BACKUP_SEMANTICS,
                            None,
                        ) {
                            let mut info: BY_HANDLE_FILE_INFORMATION = std::mem::zeroed();
                            if GetFileInformationByHandle(
                                handle_file,
                                &mut info as *mut _ as *mut _,
                            )
                            .is_ok()
                            {
                                let dev = info.dwVolumeSerialNumber as u64;
                                let ino = ((info.nFileIndexHigh as u64) << 32)
                                    | (info.nFileIndexLow as u64);
                                if crate::common_ops::check_hardlink_duplicate(opt, dev, ino) {
                                    let _ = CloseHandle(handle_file);
                                    continue;
                                }
                            }
                            let _ = CloseHandle(handle_file);
                        }
                    }
                    #[cfg(any(feature = "prof-tracy", feature = "prof-puffin"))]
                    profiling::scope!("GetCompressedFileSizeW");
                    let mut physical = logical;
                    if opt.compute_physical {
                        wide_buf.truncate(base_len);
                        wide_buf.extend_from_slice(&data.cFileName[..name_len]);
                        wide_buf.push(0);
                        let mut high: u32 = 0;
                        let low =
                            GetCompressedFileSizeW(PCWSTR(wide_buf.as_ptr()), Some(&mut high));
                        let combined = ((high as u64) << 32) | (low as u64);
                        if low != u32::MAX {
                            physical = combined;
                        }
                    }
                    crate::common_ops::update_file_stats(stat_cur, logical, physical);
                    let child = dir.join(&name);
                    ctx.report_progress(opt, Some(&child));
                }
            }
        }
        let _ = FindClose(handle);
    }
}

#[cfg(all(windows, target_env = "msvc"))]
fn try_fast_enum(
    dir: &std::path::Path,
    depth: u32,
    opt: &crate::Options,
    map: &mut StatMap,
    ctx: &ScanContext,
) -> bool {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};

    use windows::{
        core::PCWSTR,
        Wdk::Storage::FileSystem::{
            FileIdBothDirectoryInformation, NtQueryDirectoryFile, FILE_ID_BOTH_DIR_INFORMATION,
            FILE_INFORMATION_CLASS,
        },
        Win32::{
            Foundation::{CloseHandle, NTSTATUS, STATUS_NO_MORE_FILES},
            Storage::FileSystem::{
                CreateFileW, BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_NORMAL,
                FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
                OPEN_EXISTING,
            },
            System::IO::IO_STATUS_BLOCK,
        },
    };
    // Prefer fast path by default; fallback reduces risk if unsupported
    let mut path_w: Vec<u16> = dir.as_os_str().encode_wide().collect();
    if path_w.is_empty() {
        return false;
    }
    path_w.push(0);
    let h = match unsafe {
        CreateFileW(
            PCWSTR(path_w.as_ptr()),
            0x0001_0000, // FILE_LIST_DIRECTORY (use literal to avoid feature mismatch)
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL | FILE_FLAG_BACKUP_SEMANTICS,
            None,
        )
    } {
        Ok(h) => h,
        Err(_) => return false,
    };
    // Volume Serial for dedupe/FS-boundary decisions
    let mut dir_info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    let vol_serial: u64 = if unsafe {
        windows::Win32::Storage::FileSystem::GetFileInformationByHandle(
            h,
            &mut dir_info as *mut _ as *mut _,
        )
    }
    .is_ok()
    {
        dir_info.dwVolumeSerialNumber as u64
    } else {
        0
    };
    let mut buf = vec![0u8; 64 * 1024];
    let mut iosb: IO_STATUS_BLOCK = unsafe { std::mem::zeroed() };
    let stat_cur = map.entry(dir.to_path_buf()).or_default();
    loop {
        let status: NTSTATUS = unsafe {
            NtQueryDirectoryFile(
                h,
                None,
                None,
                None,
                &mut iosb as *mut _,
                buf.as_mut_ptr() as *mut _,
                buf.len() as u32,
                FILE_INFORMATION_CLASS(FileIdBothDirectoryInformation.0),
                false,
                None,
                false,
            )
        };
        if status.is_err() {
            // end or error
            if status == STATUS_NO_MORE_FILES {
                break;
            }
            let _ = unsafe { CloseHandle(h) };
            return false;
        }
        // Walk buffer
        let mut offset = 0usize;
        loop {
            if offset >= buf.len() {
                break;
            }
            let base = unsafe { buf.as_ptr().add(offset) } as *const FILE_ID_BOTH_DIR_INFORMATION;
            let info = unsafe { &*base };
            let next = info.NextEntryOffset as usize;
            // Name
            let name_len = info.FileNameLength as usize / 2;
            let name_ptr = unsafe {
                (base as *const u8).add(std::mem::size_of::<FILE_ID_BOTH_DIR_INFORMATION>())
            } as *const u16;
            let name_slice = unsafe { std::slice::from_raw_parts(name_ptr, name_len) };
            let os = std::ffi::OsString::from_wide(name_slice);
            if os == std::ffi::OsString::from(".") || os == std::ffi::OsString::from("..") {
                if next == 0 {
                    break;
                } else {
                    offset += next;
                    continue;
                }
            }
            // Exclude check (full path)
            let child = dir.join(&os);
            if crate::path_excluded(&child, opt) {
                if next == 0 {
                    break;
                } else {
                    offset += next;
                    continue;
                }
            }
            // Directory or file
            let attrs = info.FileAttributes;
            let is_dir = (attrs & 0x10) != 0; // FILE_ATTRIBUTE_DIRECTORY
            let is_reparse = (attrs & 0x400) != 0; // FILE_ATTRIBUTE_REPARSE_POINT
            if is_reparse && !opt.follow_links {
                if next == 0 {
                    break;
                } else {
                    offset += next;
                    continue;
                }
            }
            if is_dir {
                if opt.max_depth == 0 || depth < opt.max_depth {
                    // one-file-system: skip reparse directories (potential mount points)
                    if opt.one_file_system && is_reparse {
                        if next == 0 {
                            break;
                        } else {
                            offset += next;
                            continue;
                        }
                    }
                    ctx.normal_injector.push(crate::Job {
                        dir: child,
                        depth: depth + 1,
                        resume: None,
                    });
                }
            } else {
                let logical = (info.EndOfFile as i64) as u64;

                let mut physical = logical;
                if opt.compute_physical {
                    let alloc = (info.AllocationSize as i64) as u64;
                    if alloc != 0 {
                        physical = alloc;
                    }
                }
                if !opt.count_hardlinks {
                    if let Some(cache) = &opt.inode_cache {
                        // Use VolumeSerial + 64-bit FileId
                        let ino = file_id_u64(info);
                        let dev = vol_serial;
                        if cache.insert((dev, ino), ()).is_some() {
                            if next == 0 {
                                break;
                            } else {
                                offset += next;
                                continue;
                            }
                        }
                    }
                }
                if logical >= opt.min_file_size {
                    stat_cur.logical += logical;
                    stat_cur.physical += physical;
                    stat_cur.files += 1;
                    if opt.progress_every > 0 {
                        let n = ctx.total_files.fetch_add(1, Ordering::Relaxed) + 1;
                        if n % opt.progress_every == 0 {
                            if let Some(cb) = &opt.progress_callback {
                                cb(n);
                            }
                        }
                    }
                }
            }
            if next == 0 {
                break;
            } else {
                offset += next;
            }
        }
    }
    let _ = unsafe { CloseHandle(h) };
    true
}

#[cfg(not(windows))]
fn try_fast_enum(
    _dir: &std::path::Path,
    _depth: u32,
    _opt: &crate::Options,
    _map: &mut StatMap,
    _ctx: &ScanContext,
) -> bool {
    false
}

#[cfg(all(windows, not(target_env = "msvc")))]
fn try_fast_enum(
    _dir: &std::path::Path,
    _depth: u32,
    _opt: &crate::Options,
    _map: &mut StatMap,
    _ctx: &ScanContext,
) -> bool {
    false
}
#[cfg(all(windows, target_env = "msvc"))]
#[inline(always)]
fn file_id_u64(info: &windows::Wdk::Storage::FileSystem::FILE_ID_BOTH_DIR_INFORMATION) -> u64 {
    // FILE_ID_BOTH_DIR_INFORMATION.FileId is a 64-bit value. Read unaligned safely.
    unsafe { std::ptr::read_unaligned(&info.FileId as *const _ as *const u64) }
}
