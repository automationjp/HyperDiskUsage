use crate::common_ops::{
    calculate_physical_size, check_hardlink_duplicate, check_visited_directory,
    report_file_progress, update_file_stats,
};
use crate::error_handling::{last_os_error_systemcall, record_error};
use crate::{DirContext, ScanContext, StatMap};
use std::ptr::read_unaligned;
use std::sync::atomic::Ordering;

#[repr(C)]
struct AttrList {
    bitmapcount: u16,
    reserved: u16,
    commonattr: u32,
    volattr: u32,
    dirattr: u32,
    fileattr: u32,
    forkattr: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct AttrReference {
    attr_dataoffset: i32,
    attr_length: u32,
}

// Common attributes
const ATTR_BIT_MAP_COUNT: u16 = 5;
const ATTR_CMN_NAME: u32 = 0x0000_0001;
const ATTR_CMN_OBJTYPE: u32 = 0x0000_0040;
// File attributes
const ATTR_FILE_TOTALSIZE: u32 = 0x0000_0008;
const ATTR_FILE_ALLOCSIZE: u32 = 0x0000_0010;

// vnode types
const VREG: u32 = 1;
const VDIR: u32 = 2;
const VLNK: u32 = 5;

extern "C" {
    fn getattrlistbulk(
        dirfd: libc::c_int,
        attrlist: *mut AttrList,
        attrbuf: *mut libc::c_void,
        attrbufsize: libc::size_t,
        options: libc::c_ulong,
    ) -> libc::c_int;
}

pub fn process_dir(ctx: &ScanContext, dctx: &DirContext, map: &mut StatMap) {
    let dir = dctx.dir;
    let depth = dctx.depth;
    let opt = ctx.options;
    use std::ffi::{CString, OsStr};
    use std::os::unix::ffi::OsStrExt;

    let c_path = match CString::new(dir.as_os_str().as_bytes()) {
        Ok(s) => s,
        Err(_) => return,
    };
    let fd = unsafe {
        libc::open(
            c_path.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        record_error(opt, &last_os_error_systemcall(dir, "open"));
        return;
    }
    // Current dir device id
    let mut st_cur: libc::stat = unsafe { std::mem::zeroed() };
    let cur_dev: u64 = unsafe {
        if libc::fstat(fd, &mut st_cur as *mut _) == 0 {
            st_cur.st_dev as u64
        } else {
            0
        }
    };

    let mut al = AttrList {
        bitmapcount: ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: ATTR_CMN_NAME | ATTR_CMN_OBJTYPE,
        volattr: 0,
        dirattr: 0,
        fileattr: ATTR_FILE_TOTALSIZE | ATTR_FILE_ALLOCSIZE,
        forkattr: 0,
    };

    const FSOPT_NOFOLLOW: libc::c_ulong = 0x0000_0001;
    const FSOPT_NOINMEMUPDATE: libc::c_ulong = 0x0000_0002;

    fn galb_buf_size() -> usize {
        if let Ok(s) = std::env::var("HYPERDU_GALB_BUF_KB") {
            if let Ok(kb) = s.parse::<usize>() {
                return (kb.max(4)) * 1024;
            }
        }
        64 * 1024
    }

    let mut buf = vec![0u8; galb_buf_size()];
    // Pre-fetch the stats entry for current directory to avoid repeated lookups
    let stat_cur = map.entry(dir.to_path_buf()).or_default();
    unsafe {
        loop {
            #[cfg(any(feature = "prof-tracy", feature = "prof-puffin"))]
            profiling::scope!("getattrlistbulk_loop");
            let mut options: libc::c_ulong = FSOPT_NOINMEMUPDATE;
            if !opt.follow_links {
                options |= FSOPT_NOFOLLOW;
            }
            let n = getattrlistbulk(
                fd,
                &mut al as *mut _,
                buf.as_mut_ptr() as *mut _,
                buf.len(),
                options,
            );
            if n <= 0 {
                break;
            }
            let mut offset = 0usize;
            for _ in 0..n {
                if offset + 4 > buf.len() {
                    break;
                }
                let rec_base = unsafe { buf.as_ptr().add(offset) };
                let reclen = read_unaligned(rec_base as *const u32) as usize;
                if reclen == 0 || offset + reclen > buf.len() {
                    break;
                }
                let mut cursor = unsafe { rec_base.add(4) };

                // common: name (attrreference)
                let name_ref = read_unaligned(cursor as *const AttrReference);
                cursor = unsafe { cursor.add(std::mem::size_of::<AttrReference>()) };
                // common: objtype (u32)
                let objtype = read_unaligned(cursor as *const u32);
                cursor = unsafe { cursor.add(std::mem::size_of::<u32>()) };
                // file: totalsize (u64), allocsize (u64)
                let totalsize = read_unaligned(cursor as *const u64);
                cursor = unsafe { cursor.add(std::mem::size_of::<u64>()) };
                let allocsize = read_unaligned(cursor as *const u64);

                // name pointer
                let name_ptr = unsafe { rec_base.add(name_ref.attr_dataoffset as usize) };
                let name_len = name_ref.attr_length as usize;
                let name_slice = unsafe { std::slice::from_raw_parts(name_ptr, name_len) };
                // Trim trailing NUL if present
                let name_slice = if !name_slice.is_empty() && *name_slice.last().unwrap() == 0 {
                    &name_slice[..name_slice.len() - 1]
                } else {
                    name_slice
                };
                if name_slice == b"." || name_slice == b".." {
                    offset += reclen;
                    continue;
                }
                if crate::name_matches(name_slice, opt) {
                    offset += reclen;
                    continue;
                }

                let is_dir = objtype == VDIR;
                let is_lnk = objtype == VLNK;
                if is_lnk && !opt.follow_links {
                    offset += reclen;
                    continue;
                }

                let child = dir.join(OsStr::from_bytes(name_slice));
                if crate::path_excluded(&child, opt) {
                    offset += reclen;
                    continue;
                }

                if is_dir {
                    if opt.max_depth == 0 || depth < opt.max_depth {
                    ctx.enqueue_dir(child.clone(), depth + 1);
                    }
                } else {
                    // Hardlink dedupe
                    if let Ok(c_child) = CString::new(child.as_os_str().as_bytes()) {
                        let mut st: libc::stat = unsafe { std::mem::zeroed() };
                        let rc = unsafe { libc::lstat(c_child.as_ptr(), &mut st) };
                        if rc == 0 {
                            let dev = st.st_dev as u64;
                            let ino = st.st_ino as u64;
                            if check_hardlink_duplicate(opt, dev, ino) {
                                offset += reclen;
                                continue;
                            }
                        }
                    }
                    let logical = totalsize as u64;
                    if logical >= opt.min_file_size {
                        let physical = if opt.compute_physical {
                            if allocsize == 0 {
                                logical
                            } else {
                                allocsize as u64
                            }
                        } else {
                            logical
                        };
                        update_file_stats(stat_cur, logical, physical);
                        report_file_progress(opt, ctx.total_files, Some(&child));
                    }
                }

                // one-file-system and loop check for directories
                if is_dir && opt.one_file_system {
                    let mut st: libc::stat = unsafe { std::mem::zeroed() };
                    let c_child = match CString::new(child.as_os_str().as_bytes()) {
                        Ok(s) => s,
                        Err(_) => {
                            offset += reclen;
                            continue;
                        }
                    };
                    let rc = unsafe { libc::lstat(c_child.as_ptr(), &mut st) };
                    if rc == 0 {
                        if (st.st_dev as u64) != cur_dev {
                            offset += reclen;
                            continue;
                        }
                        let dev = st.st_dev as u64;
                        let ino = st.st_ino as u64;
                        if check_visited_directory(opt, dev, ino) {
                            offset += reclen;
                            continue;
                        }
                    }
                }

                offset += reclen;
            }
        }
        libc::close(fd);
    }
}
