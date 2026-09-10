use std::{
    ffi::{CString, OsStr},
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::ffi::OsStrExt,
    },
    sync::atomic::Ordering,
};

mod strict;
#[cfg(all(target_env = "gnu", feature = "linux-io-uring"))]
mod uring;
#[cfg(target_env = "gnu")]
pub(super) mod xfs_bulk;

use super::linux_helpers::{decode_dirent, DirEntry, FileCounter};
use crate::{
    common_ops::{
        calculate_physical_size, check_hardlink_duplicate, check_visited_directory,
        should_fast_exclude, update_file_stats,
    },
    error_handling::{last_os_error_systemcall, record_error, ScanError},
    memory_pool::BufferGuard,
    name_matches, DirContext, ScanContext, StatMap,
};

const BATCH_SIZE: usize = 64;

pub fn process_dir(ctx: &ScanContext, dctx: &DirContext, map: &mut StatMap) {
    let dir = dctx.dir;
    let depth = dctx.depth;
    let opt = ctx.options;
    let strict_accounting = matches!(
        opt.compat_mode,
        crate::CompatMode::GnuStrict | crate::CompatMode::PosixStrict
    );
    if opt.cancel.load(Ordering::Relaxed) {
        return;
    }
    let c_path = match CString::new(dir.as_os_str().as_bytes()) {
        Ok(path) => path,
        Err(error) => {
            record_error(
                opt,
                &ScanError::IoError {
                    path: dir.to_path_buf(),
                    source: std::io::Error::new(std::io::ErrorKind::InvalidInput, error),
                },
            );
            return;
        }
    };
    let raw = super::linux_helpers::open_dir_readonly(&c_path, opt.follow_links);
    if raw < 0 {
        record_error(opt, &last_os_error_systemcall(dir, "open"));
        return;
    }
    // SAFETY: open returned a new descriptor, owned by this directory job.
    let directory = unsafe { OwnedFd::from_raw_fd(raw) };
    let fd = directory.as_raw_fd();
    #[cfg(target_env = "gnu")]
    let xfs_cache = opt
        .xfs_bulk_cache
        .as_ref()
        .filter(|cache| cache.is_read_only());
    #[cfg(target_env = "gnu")]
    let need_device = xfs_cache.is_some();
    #[cfg(not(target_env = "gnu"))]
    let need_device = false;
    let mut directory_blocks = None;
    #[allow(unused_variables, unused_assignments)]
    let mut directory_dev = 0;
    if strict_accounting || opt.one_file_system || crate::follows_links(opt) || need_device {
        // SAFETY: stat is an integer-only output, valid when zeroed.
        let mut metadata: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: fd is owned/live and metadata is writable.
        if unsafe { libc::fstat(fd, &mut metadata) } != 0 {
            record_error(opt, &last_os_error_systemcall(dir, "fstat"));
            return;
        }
        let dev = super::linux_helpers::packed_dev(metadata.st_dev);
        directory_dev = dev;
        if (opt.one_file_system && opt.root_fs_id != 0 && dev != opt.root_fs_id)
            || (dctx.resume.is_none() && check_visited_directory(opt, dev, metadata.st_ino))
        {
            return;
        }
        if strict_accounting && dctx.resume.is_none() {
            directory_blocks = Some(metadata.st_blocks.max(0) as u64);
        }
    }
    #[cfg(feature = "prefetch-advise")]
    if opt.io_prefetch {
        // SAFETY: hints use a live descriptor and do not access Rust memory.
        unsafe {
            let _ = libc::posix_fadvise(fd, 0, 0, libc::POSIX_FADV_SEQUENTIAL);
            let _ = libc::readahead(fd, 0, 1 << 20);
        }
    }
    if let Some(offset) = dctx.resume {
        // SAFETY: seek uses this job's owned descriptor.
        if unsafe { libc::lseek(fd, offset as libc::off_t, libc::SEEK_SET) } < 0 {
            record_error(opt, &last_os_error_systemcall(dir, "lseek"));
            return;
        }
    }
    // Construct lazily: tiny directories never pay io_uring setup/probe cost.
    #[cfg(all(target_env = "gnu", feature = "linux-io-uring"))]
    let mut batcher = None;
    #[cfg(all(target_env = "gnu", feature = "linux-io-uring"))]
    let mut try_uring = opt.use_io_uring;
    let mut guard = BufferGuard::borrow(opt.getdents_buf_bytes);
    let buffer = guard.as_mut_slice();
    let stat = map.entry(dir.to_path_buf()).or_default();
    if let Some(blocks) = directory_blocks {
        stat.physical += calculate_physical_size(opt, 0, blocks);
    }
    let fast_exclude = should_fast_exclude(opt);
    let mut counted = FileCounter::new(opt);
    let mut processed = 0usize;
    'read: loop {
        if opt.cancel.load(Ordering::Relaxed) {
            break;
        }
        // SAFETY: getdents64 writes at most buffer.len() bytes to this live buffer.
        let bytes =
            unsafe { libc::syscall(libc::SYS_getdents64, fd, buffer.as_mut_ptr(), buffer.len()) }
                as isize;
        if bytes < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            record_error(opt, &last_os_error_systemcall(dir, "getdents64"));
            break;
        }
        if bytes == 0 {
            break;
        }
        let bytes = bytes as usize;
        if bytes > buffer.len() {
            record_error(
                opt,
                &ScanError::IoError {
                    path: dir.to_path_buf(),
                    source: std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "getdents64 length exceeds buffer",
                    ),
                },
            );
            break;
        }
        let mut offset = 0;
        while offset < bytes {
            if opt.cancel.load(Ordering::Relaxed) {
                break 'read;
            }
            let yield_every = opt.dir_yield_every.load(Ordering::Relaxed);
            let limit = if yield_every == 0 {
                BATCH_SIZE
            } else {
                BATCH_SIZE.min(yield_every - processed % yield_every)
            };
            let mut entries = Vec::with_capacity(limit);
            while offset < bytes && entries.len() < limit {
                let entry = match decode_dirent(&buffer[offset..bytes]) {
                    Ok(entry) => entry,
                    Err(source) => {
                        record_error(
                            opt,
                            &ScanError::IoError {
                                path: dir.to_path_buf(),
                                source,
                            },
                        );
                        break 'read;
                    }
                };
                offset += entry.record_len;
                let name = entry.name.to_bytes();
                if name == b"."
                    || name == b".."
                    || name_matches(name, opt)
                    || (!fast_exclude
                        && crate::path_excluded(&dir.join(OsStr::from_bytes(name)), opt))
                    || (entry.kind == libc::DT_LNK && !opt.follow_links && !strict_accounting)
                {
                    continue;
                }
                entries.push(entry);
            }
            if entries.is_empty() {
                continue;
            }
            #[allow(unused_mut)]
            let mut metadata = vec![None; entries.len()];
            #[cfg(target_env = "gnu")]
            if let Some(cache) = xfs_cache {
                for (entry, result) in entries.iter().zip(&mut metadata) {
                    if needs_metadata(entry, opt, strict_accounting) {
                        if let Some(value) = cache.lookup(directory_dev, entry.ino) {
                            // A cache describes the link itself, never its target.
                            if !(opt.follow_links && value.mode & libc::S_IFMT == libc::S_IFLNK) {
                                *result = Some(strict::Metadata {
                                    logical: value.logical,
                                    blocks: value.blocks512,
                                    dev: value.dev,
                                    ino: value.ino,
                                    nlink: value.nlink,
                                    mode: value.mode,
                                });
                            }
                        }
                    }
                }
            }
            #[cfg(all(target_env = "gnu", feature = "linux-io-uring"))]
            {
                let pending: Vec<_> = entries
                    .iter()
                    .enumerate()
                    .filter(|(index, entry)| {
                        metadata[*index].is_none() && needs_metadata(entry, opt, strict_accounting)
                    })
                    .map(|(index, _)| index)
                    .collect();
                if try_uring && pending.len() >= 8 {
                    try_uring = false;
                    match uring::StatxBatcher::new(fd) {
                        Ok(value) => batcher = value,
                        Err(error) => {
                            log::debug!("io_uring unavailable; synchronous metadata: {error}")
                        }
                    }
                }
                if let Some(active) = batcher.as_mut() {
                    let names: Vec<_> = pending.iter().map(|index| entries[*index].name).collect();
                    let flags = strict::flags(opt.follow_links)
                        | if strict_accounting {
                            0
                        } else {
                            libc::AT_STATX_DONT_SYNC
                        };
                    match active.query(&names, flags, strict::REQUIRED, &opt.cancel) {
                        Ok(results) => {
                            for (index, result) in pending.into_iter().zip(results) {
                                // Incomplete masks and failed CQEs use the same exact fallback.
                                if let Ok(value) = strict::statx_or_fstatat(
                                    fd,
                                    entries[index].name,
                                    opt.follow_links,
                                    result,
                                ) {
                                    metadata[index] = Some(value);
                                }
                                // A final lookup failure is recorded by the synchronous path.
                            }
                        }
                        Err(_) if opt.cancel.load(Ordering::Relaxed) => break 'read,
                        Err(error) => {
                            log::warn!("io_uring batch failed; synchronous metadata: {error}");
                            // Retain kernel-referenced resources if draining failed.
                            batcher = None;
                        }
                    }
                }
            }
            let last_offset = entries.last().expect("nonempty batch").offset;
            for (entry, cached) in entries.iter().zip(metadata) {
                if opt.cancel.load(Ordering::Relaxed) {
                    break 'read;
                }
                let name = entry.name.to_bytes();
                if entry.kind == libc::DT_DIR {
                    if opt.max_depth == 0 || depth < opt.max_depth {
                        ctx.enqueue_dir(dir.join(OsStr::from_bytes(name)), depth + 1);
                    }
                } else if !needs_metadata(entry, opt, strict_accounting) {
                    update_file_stats(stat, 4096, 4096);
                    counted.record(name, 4096, 4096);
                } else {
                    let result = match cached {
                        Some(value) => Ok(value),
                        None => {
                            strict::metadata(fd, entry.name, opt.follow_links, strict_accounting)
                        }
                    };
                    match result {
                        Ok(value) if value.is_dir() => {
                            if opt.max_depth == 0 || depth < opt.max_depth {
                                ctx.enqueue_dir(dir.join(OsStr::from_bytes(name)), depth + 1);
                            }
                        }
                        Ok(value)
                            if (strict_accounting || value.is_reg())
                                && value.logical >= opt.min_file_size =>
                        {
                            if !(crate::common_ops::hardlink_candidate(opt, value.nlink)
                                && check_hardlink_duplicate(opt, value.dev, value.ino))
                            {
                                let physical =
                                    calculate_physical_size(opt, value.logical, value.blocks);
                                update_file_stats(stat, value.logical, physical);
                                counted.record(name, value.logical, physical);
                            }
                        }
                        Ok(_) => {}
                        Err(source) => record_error(
                            opt,
                            &ScanError::IoError {
                                path: dir.join(OsStr::from_bytes(name)),
                                source,
                            },
                        ),
                    }
                }
                processed += 1;
            }
            if yield_every > 0 && processed % yield_every == 0 {
                counted.flush(ctx, opt, dir);
                ctx.enqueue_resume(dir.to_path_buf(), depth, last_offset);
                return;
            }
        }
    }
    counted.flush(ctx, opt, dir);
}

fn needs_metadata(entry: &DirEntry<'_>, opt: &crate::Options, strict: bool) -> bool {
    entry.kind != libc::DT_DIR
        && (strict
            || entry.kind != libc::DT_REG
            || opt.compute_physical
            || !opt.approximate_sizes
            || opt.min_file_size != 0)
}
