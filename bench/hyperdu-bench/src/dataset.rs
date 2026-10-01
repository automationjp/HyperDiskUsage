//! Independent, deliberately unoptimized metadata oracle. Never calls HyperDU.
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Oracle {
    pub files: u64,
    pub entries: u64,
    pub directories: u64,
    pub logical: u64,
    pub fingerprint: String,
}

#[cfg(unix)]
fn identity(path: &Path, m: &fs::Metadata) -> Result<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let _ = path;
    Ok((m.dev(), m.ino()))
}
#[cfg(windows)]
fn identity(path: &Path, _m: &fs::Metadata) -> Result<(u64, u64)> {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    };
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(0x02000000)
        .open(path)?;
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    ensure!(
        unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } != 0,
        "cannot read file identity"
    );
    Ok((
        info.dwVolumeSerialNumber as u64,
        (info.nFileIndexHigh as u64) << 32 | info.nFileIndexLow as u64,
    ))
}

fn path_bytes(p: &Path) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        p.as_os_str().as_bytes().to_vec()
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        p.as_os_str()
            .encode_wide()
            .flat_map(u16::to_le_bytes)
            .collect()
    }
}

pub fn oracle(root: &Path) -> Result<Oracle> {
    ensure!(root.is_dir(), "dataset must be a directory");
    let device = identity(root, &fs::symlink_metadata(root)?)?.0;
    let mut out = Oracle {
        files: 0,
        entries: 0,
        directories: 0,
        logical: 0,
        fingerprint: String::new(),
    };
    let mut hash = Sha256::new();
    let mut seen = HashSet::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(p) = stack.pop() {
        let m = fs::symlink_metadata(&p)
            .with_context(|| format!("oracle metadata: {}", p.display()))?;
        ensure!(
            !m.file_type().is_symlink(),
            "v1 contract does not accept symbolic links"
        );
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            ensure!(
                m.file_attributes() & 0x400 == 0,
                "v1 rejects reparse points"
            );
        }
        let id = identity(&p, &m)?;
        ensure!(id.0 == device, "v1 rejects nested filesystem mounts");
        let name = path_bytes(p.strip_prefix(root)?);
        hash.update((name.len() as u64).to_le_bytes());
        hash.update(name);
        hash.update(id.0.to_le_bytes());
        hash.update(id.1.to_le_bytes());
        hash.update(m.len().to_le_bytes());
        hash.update([u8::from(m.is_dir())]);
        hash.update(
            m.modified()?
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
                .to_le_bytes(),
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            hash.update(m.ctime().to_le_bytes());
            hash.update(m.ctime_nsec().to_le_bytes());
            hash.update(m.mode().to_le_bytes());
            hash.update(m.nlink().to_le_bytes());
            hash.update(m.blocks().to_le_bytes());
        }
        if m.is_dir() {
            out.directories += 1;
            let mut children = fs::read_dir(&p)?
                .map(|e| e.map(|e| e.path()))
                .collect::<std::io::Result<Vec<_>>>()?;
            children.sort();
            children.reverse();
            stack.extend(children);
        } else {
            ensure!(m.is_file(), "v1 rejects non-regular files");
            out.entries += 1;
            if seen.insert(id) {
                out.files += 1;
                out.logical = out
                    .logical
                    .checked_add(m.len())
                    .context("logical byte overflow")?;
            }
        }
    }
    out.fingerprint = format!("{:x}", hash.finalize());
    Ok(out)
}

pub fn generate(root: &Path, shape: &str, count: u64, bytes: usize) -> Result<Oracle> {
    ensure!(
        !root.exists(),
        "fixture path already exists; refusing to overwrite"
    );
    ensure!(
        matches!(shape, "flat" | "wide" | "deep" | "skewed"),
        "unknown shape"
    );
    ensure!(
        count <= 10_000_000
            && bytes <= 1024 * 1024
            && count.saturating_mul(bytes as u64) <= 64 * 1024 * 1024 * 1024,
        "fixture exceeds safety budget"
    );
    fs::create_dir(root)?;
    let payload: Vec<_> = (0..bytes).map(|i| (i % 251) as u8).collect();
    for i in 0..count {
        let mut dir = root.to_path_buf();
        match shape {
            "wide" => {
                dir.push(format!("d{:04}", i % 100));
            }
            "deep" => {
                for _ in 0..(i % 32) {
                    dir.push("d");
                }
            }
            "skewed" => {
                dir.push(if i % 10 == 0 {
                    format!("small{:03}", i % 100)
                } else {
                    "large".into()
                });
            }
            _ => {}
        }
        fs::create_dir_all(&dir)?;
        fs::write(dir.join(format!("f{i:010}.bin")), &payload)?;
    }
    oracle(root)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shapes_have_exact_counts_and_refuse_overwrite() {
        let tmp = tempfile::tempdir().unwrap();
        for shape in ["flat", "wide", "deep", "skewed"] {
            let root = tmp.path().join(shape);
            let o = generate(&root, shape, 103, 17).unwrap();
            assert_eq!((o.files, o.entries, o.logical), (103, 103, 1751));
            assert_eq!(o, oracle(&root).unwrap());
            assert!(generate(&root, shape, 1, 1).is_err());
        }
    }
    #[test]
    fn mutations_with_same_total_change_fingerprint() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("data");
        let old = generate(&p, "flat", 2, 8).unwrap();
        fs::rename(p.join("f0000000000.bin"), p.join("renamed.bin")).unwrap();
        let new = oracle(&p).unwrap();
        assert_eq!(old.logical, new.logical);
        assert_ne!(old.fingerprint, new.fingerprint);
    }
    #[cfg(unix)]
    #[test]
    fn hardlinks_fold_and_symlinks_fail_closed() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("a"), b"abc").unwrap();
        fs::hard_link(tmp.path().join("a"), tmp.path().join("b")).unwrap();
        let o = oracle(tmp.path()).unwrap();
        assert_eq!((o.entries, o.files, o.logical), (2, 1, 3));
        std::os::unix::fs::symlink("a", tmp.path().join("link")).unwrap();
        assert!(oracle(tmp.path()).is_err());
    }
}
