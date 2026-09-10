//! Exact metadata required by the glibc strict accounting path.
use std::{ffi::CStr, io};

const REQUIRED: u32 = libc::STATX_TYPE
    | libc::STATX_MODE
    | libc::STATX_SIZE
    | libc::STATX_BLOCKS
    | libc::STATX_INO
    | libc::STATX_NLINK;

pub(super) struct Metadata {
    pub logical: u64,
    pub blocks: u64,
    pub dev: u64,
    pub ino: u64,
    pub nlink: u32,
    mode: u32,
}

impl Metadata {
    pub fn is_dir(&self) -> bool {
        self.mode & libc::S_IFMT == libc::S_IFDIR
    }
}

pub(super) fn metadata(fd: libc::c_int, name: &CStr, follow_links: bool) -> io::Result<Metadata> {
    // SAFETY: libc::statx is a C integer-only output structure; zero is valid.
    let mut stx: libc::statx = unsafe { std::mem::zeroed() };
    let flags = flags(follow_links);
    // Strict accounting requests synchronized metadata and verifies every field
    // it consumes. A successful syscall alone does not guarantee those fields.
    // SAFETY: CStr supplies a live NUL-terminated name, and stx is writable.
    let rc = unsafe { libc::statx(fd, name.as_ptr(), flags, REQUIRED, &mut stx) };
    let result = if rc == 0 {
        Ok(stx)
    } else {
        Err(io::Error::last_os_error())
    };
    statx_or_fstatat(fd, name, follow_links, result)
}

fn flags(follow_links: bool) -> libc::c_int {
    libc::AT_NO_AUTOMOUNT
        | if follow_links {
            0
        } else {
            libc::AT_SYMLINK_NOFOLLOW
        }
}

fn statx_or_fstatat(
    fd: libc::c_int,
    name: &CStr,
    follow_links: bool,
    result: io::Result<libc::statx>,
) -> io::Result<Metadata> {
    if let Ok(stx) = result {
        if stx.stx_mask & REQUIRED == REQUIRED {
            return Ok(Metadata {
                logical: stx.stx_size,
                blocks: stx.stx_blocks,
                dev: ((stx.stx_dev_major as u64) << 32) | stx.stx_dev_minor as u64,
                ino: stx.stx_ino,
                nlink: stx.stx_nlink,
                mode: stx.stx_mode as u32,
            });
        }
    }
    // The fallback must preserve follow policy, inode identity and allocated
    // blocks. Using logical length here would overcount sparse files.
    // SAFETY: libc::stat is a C integer-only output structure; zero is valid.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: CStr supplies a live NUL-terminated name, and st is writable.
    if unsafe { libc::fstatat(fd, name.as_ptr(), &mut st, flags(follow_links)) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(Metadata {
        logical: st.st_size.max(0) as u64,
        blocks: st.st_blocks.max(0) as u64,
        dev: crate::platform::linux_helpers::packed_dev(st.st_dev),
        ino: st.st_ino,
        nlink: st.st_nlink.min(u32::MAX as _) as u32,
        mode: st.st_mode,
    })
}

#[cfg(test)]
mod tests {
    use std::{ffi::CString, fs, os::unix::fs::symlink};

    use super::*;

    #[test]
    fn complete_statx_preserves_zero_blocks_without_fallback() {
        let mut stx: libc::statx = unsafe { std::mem::zeroed() };
        stx.stx_mask = REQUIRED;
        stx.stx_mode = libc::S_IFREG as u16;
        stx.stx_size = 8 * 1024 * 1024;
        stx.stx_ino = 23;
        stx.stx_nlink = 2;
        stx.stx_dev_major = 8;
        stx.stx_dev_minor = 1;
        let name = CString::new("unused").unwrap();
        let md = statx_or_fstatat(-1, &name, false, Ok(stx)).unwrap();
        assert_eq!(
            (md.logical, md.blocks, md.ino, md.nlink),
            (8 * 1024 * 1024, 0, 23, 2)
        );
        assert_eq!(md.dev, (8 << 32) | 1);
        assert!(!md.is_dir());
    }

    #[test]
    fn failed_or_incomplete_statx_falls_back_with_sparse_identity_and_link_policy() {
        use std::os::unix::{ffi::OsStrExt, fs::MetadataExt};
        let fixture = tempfile::tempdir().unwrap();
        let file = fixture.path().join("sparse");
        fs::File::create(&file)
            .unwrap()
            .set_len(8 * 1024 * 1024)
            .unwrap();
        fs::hard_link(&file, fixture.path().join("hardlink")).unwrap();
        let link = fixture.path().join("link");
        symlink("sparse", &link).unwrap();
        for path in [&file, &link] {
            let name = CString::new(path.as_os_str().as_bytes()).unwrap();
            for follow in [false, true] {
                let expected = if follow {
                    fs::metadata(path)
                } else {
                    fs::symlink_metadata(path)
                }
                .unwrap();
                // Every required missing field must independently force fallback.
                for missing in [
                    libc::STATX_TYPE,
                    libc::STATX_MODE,
                    libc::STATX_SIZE,
                    libc::STATX_BLOCKS,
                    libc::STATX_INO,
                    libc::STATX_NLINK,
                    0,
                ] {
                    let mut stx: libc::statx = unsafe { std::mem::zeroed() };
                    stx.stx_mask = REQUIRED & !missing;
                    let result = if missing == 0 {
                        Err(io::Error::from_raw_os_error(libc::ENOSYS))
                    } else {
                        Ok(stx)
                    };
                    let md = statx_or_fstatat(libc::AT_FDCWD, &name, follow, result).unwrap();
                    assert_eq!(
                        (md.logical, md.blocks, md.ino, md.nlink),
                        (
                            expected.len(),
                            expected.blocks(),
                            expected.ino(),
                            expected.nlink() as u32
                        )
                    );
                    assert_eq!(
                        md.dev,
                        crate::platform::linux_helpers::packed_dev(expected.dev())
                    );
                    assert_eq!(md.mode, expected.mode());
                }
            }
        }
    }

    #[test]
    fn fallback_failure_preserves_errno_and_dangling_link_policy() {
        use std::os::unix::ffi::OsStrExt;
        let fixture = tempfile::tempdir().unwrap();
        let link = fixture.path().join("dangling");
        symlink("absent", &link).unwrap();
        let name = CString::new(link.as_os_str().as_bytes()).unwrap();
        let missing_statx = || Err(io::Error::from_raw_os_error(libc::ENOSYS));
        let md = statx_or_fstatat(libc::AT_FDCWD, &name, false, missing_statx()).unwrap();
        assert_eq!(md.mode & libc::S_IFMT, libc::S_IFLNK);
        assert_eq!(md.logical, 6);
        let error = statx_or_fstatat(libc::AT_FDCWD, &name, true, missing_statx())
            .err()
            .unwrap();
        assert_eq!(error.raw_os_error(), Some(libc::ENOENT));
    }
}
