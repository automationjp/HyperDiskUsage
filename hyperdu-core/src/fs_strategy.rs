use std::path::Path;

use crate::Options;

pub trait FileSystemStrategy: Send + Sync {
    fn name(&self) -> &'static str;
    fn apply(&self, opt: &mut Options, report: &mut Vec<String>) -> FsApplyOutcome;
}

struct GenericStrategy;
impl FileSystemStrategy for GenericStrategy {
    fn name(&self) -> &'static str {
        "generic"
    }
    fn apply(&self, _opt: &mut Options, _report: &mut Vec<String>) -> FsApplyOutcome {
        // Keep defaults
        FsApplyOutcome {
            recommended_threads: None,
            recommend_logical_only: false,
        }
    }
}

/// Native metadata APIs already return identity and allocation in batches.
/// Selection must not widen the opt-in raw-volume backend or change accounting.
struct NativeStrategy(&'static str);
impl FileSystemStrategy for NativeStrategy {
    fn name(&self) -> &'static str {
        self.0
    }
    fn apply(&self, _opt: &mut Options, _report: &mut Vec<String>) -> FsApplyOutcome {
        FsApplyOutcome {
            recommended_threads: None,
            recommend_logical_only: false,
        }
    }
}

struct Ext4Strategy;
impl FileSystemStrategy for Ext4Strategy {
    fn name(&self) -> &'static str {
        "ext4"
    }
    fn apply(&self, opt: &mut Options, report: &mut Vec<String>) -> FsApplyOutcome {
        // Favor larger dirent buffer on fast storage
        opt.getdents_buf_bytes = 128 * 1024;
        report.push("getdents_buf_kb=128".into());
        FsApplyOutcome {
            recommended_threads: None,
            recommend_logical_only: false,
        }
    }
}

struct XfsStrategy;
impl FileSystemStrategy for XfsStrategy {
    fn name(&self) -> &'static str {
        "xfs"
    }
    fn apply(&self, opt: &mut Options, report: &mut Vec<String>) -> FsApplyOutcome {
        opt.getdents_buf_bytes = 128 * 1024;
        report.push("getdents_buf_kb=128".into());
        FsApplyOutcome {
            recommended_threads: None,
            recommend_logical_only: false,
        }
    }
}

struct BtrfsStrategy;
impl FileSystemStrategy for BtrfsStrategy {
    fn name(&self) -> &'static str {
        "btrfs"
    }
    fn apply(&self, opt: &mut Options, report: &mut Vec<String>) -> FsApplyOutcome {
        // Logical bytes can be cheaper on CoW storage, but changing the metric
        // is a user decision, never an automatic performance optimization.
        opt.getdents_buf_bytes = 128 * 1024;
        report.push("getdents_buf_kb=128".into());
        FsApplyOutcome {
            recommended_threads: None,
            recommend_logical_only: true,
        }
    }
}

struct ZfsStrategy;
impl FileSystemStrategy for ZfsStrategy {
    fn name(&self) -> &'static str {
        "zfs"
    }
    fn apply(&self, opt: &mut Options, report: &mut Vec<String>) -> FsApplyOutcome {
        opt.getdents_buf_bytes = 128 * 1024;
        report.push("getdents_buf_kb=128".into());
        FsApplyOutcome {
            recommended_threads: None,
            recommend_logical_only: false,
        }
    }
}

struct DrvfsStrategy;
impl FileSystemStrategy for DrvfsStrategy {
    fn name(&self) -> &'static str {
        "drvfs"
    }
    fn apply(&self, opt: &mut Options, report: &mut Vec<String>) -> FsApplyOutcome {
        // WSL DrvFS: reduce parallel pressure while preserving size accounting.
        // Slightly smaller buffer (context switch heavy)
        opt.getdents_buf_bytes = 64 * 1024;
        report.push("getdents_buf_kb=64".into());
        // Suggest fewer threads
        FsApplyOutcome {
            recommended_threads: Some(4),
            recommend_logical_only: false,
        }
    }
}

struct NetworkStrategy;
impl FileSystemStrategy for NetworkStrategy {
    fn name(&self) -> &'static str {
        "network"
    }
    fn apply(&self, opt: &mut Options, report: &mut Vec<String>) -> FsApplyOutcome {
        // Limit pressure without replacing allocated bytes with logical bytes.
        opt.getdents_buf_bytes = 64 * 1024;
        report.push("getdents_buf_kb=64".into());
        // Optionally reduce threads in caller if needed (not adjusted here)
        FsApplyOutcome {
            recommended_threads: Some(4),
            recommend_logical_only: false,
        }
    }
}

/// Whether `path` is at or below the mount point `mp`.
///
/// A plain string prefix test is wrong: it makes `/mnt/foo` look like it lives
/// under the mount `/mnt/f`, which then picks that mount's strategy and can
/// silently switch the whole scan to logical sizes. The match has to land on a
/// path separator, or consume the whole path.
#[cfg(target_os = "linux")]
fn path_under_mount(path: &str, mp: &str) -> bool {
    if mp == "/" {
        return path.starts_with('/');
    }
    let Some(rest) = path.strip_prefix(mp) else {
        return false;
    };
    rest.is_empty() || rest.starts_with('/')
}

/// Decode the octal escapes the kernel writes for characters that would
/// otherwise break the field split (`\040` space, `\011` tab, `\012` newline,
/// `\134` backslash). Applied to mount points only; the source field is never
/// compared against a path and must not be rewritten.
#[cfg(target_os = "linux")]
fn unescape_mount_field(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 3 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 4], 8) {
                out.push(v as char);
                i += 4;
                continue;
            }
        }
        out.push(b[i] as char);
        i += 1;
    }
    out
}

#[cfg(target_os = "linux")]
fn fs_type_for_path_linux(p: &Path) -> Option<String> {
    use std::fs;
    let path = fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let files = ["/proc/self/mountinfo", "/proc/mounts", "/etc/mtab"]; // best-effort
    for m in files {
        if let Ok(text) = fs::read_to_string(m) {
            let mut best: Option<(usize, String)> = None; // (match_len, fstype)
            for line in text.lines() {
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                // mountinfo: many fields, mount point is 5th (post-root fields), fstype later; mounts: src mp fstype ...
                if m.ends_with("mountinfo") {
                    // format: ID parent major:minor root mount point options - fstype src opts
                    // Split by ' - ' then take second part's first token as fstype
                    if let Some(idx) = line.find(" - ") {
                        let (pre, post) = line.split_at(idx);
                        let pre_parts: Vec<&str> = pre.split_whitespace().collect();
                        if pre_parts.len() >= 5 {
                            let mp = unescape_mount_field(pre_parts[4]);
                            if path_under_mount(&path.to_string_lossy(), &mp) {
                                let post_parts: Vec<&str> = post[3..].split_whitespace().collect();
                                if !post_parts.is_empty() {
                                    let fs = post_parts[0].to_string();
                                    let l = mp.len();
                                    if best.as_ref().map(|(bl, _)| l > *bl).unwrap_or(true) {
                                        best = Some((l, fs));
                                    }
                                }
                            }
                        }
                    }
                } else {
                    // mounts/mtab: src mp fstype ...
                    let parts: Vec<&str> = line.split_whitespace().collect();
                    if parts.len() >= 3 {
                        let mp = unescape_mount_field(parts[1]);
                        let fs = parts[2];
                        if path_under_mount(&path.to_string_lossy(), &mp) {
                            let l = mp.len();
                            if best.as_ref().map(|(bl, _)| l > *bl).unwrap_or(true) {
                                best = Some((l, fs.to_string()));
                            }
                        }
                    }
                }
            }
            if let Some((_, fs)) = best {
                return Some(fs);
            }
        }
    }
    None
}

#[cfg(target_os = "macos")]
fn fs_type_for_path_macos(path: &Path) -> Option<String> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    let path = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut info: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: both pointers remain live and info has the Darwin statfs layout.
    if unsafe { libc::statfs(path.as_ptr(), &mut info) } != 0 {
        return None;
    }
    let end = info.f_fstypename.iter().position(|&byte| byte == 0)?;
    let bytes: Vec<u8> = info.f_fstypename[..end].iter().map(|&v| v as u8).collect();
    String::from_utf8(bytes).ok()
}

#[cfg(windows)]
fn fs_type_for_path_windows(path: &Path) -> Option<String> {
    use std::os::windows::ffi::OsStrExt;

    use windows::{
        core::PCWSTR,
        Win32::Storage::FileSystem::{GetDriveTypeW, GetVolumeInformationW, GetVolumePathNameW},
    };
    // Resolve relative paths and junctions before asking for their mount point.
    // GetVolumePathNameW otherwise returns the boot drive for relative paths.
    let resolved = path.canonicalize().ok()?;
    if matches!(
        resolved.components().next(),
        Some(std::path::Component::Prefix(prefix))
            if matches!(prefix.kind(), std::path::Prefix::UNC(_, _) | std::path::Prefix::VerbatimUNC(_, _))
    ) {
        // SMB does not implement all volume-management APIs. A resolved UNC
        // path already establishes remote access even when that query fails.
        return Some("remote".into());
    }
    let wide: Vec<u16> = resolved.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut mount = vec![0u16; 32768];
    // SAFETY: input is terminated, and the API receives the output slice length.
    unsafe { GetVolumePathNameW(PCWSTR(wide.as_ptr()), &mut mount) }.ok()?;
    let end = mount.iter().position(|&unit| unit == 0)?;
    if end == 0 {
        return None;
    }
    mount.truncate(end + 1);
    // DRIVE_REMOTE includes SMB and other redirectors. Do not label an unknown
    // remote protocol NTFS merely because the server stores its data on NTFS.
    if unsafe { GetDriveTypeW(PCWSTR(mount.as_ptr())) } == 4 {
        return Some("remote".into());
    }
    let mut name = [0u16; 256];
    // SAFETY: mount is NUL-terminated; the bounded name slice is the only output.
    unsafe {
        GetVolumeInformationW(
            PCWSTR(mount.as_ptr()),
            None,
            None,
            None,
            None,
            Some(&mut name),
        )
    }
    .ok()?;
    let end = name.iter().position(|&unit| unit == 0)?;
    String::from_utf16(&name[..end]).ok()
}

/// Detect the filesystem of a resolved path without changing scan options.
/// Unknown or inaccessible filesystems return None; remote Windows volumes
/// are reported as remote because their server filesystem is not a capability.
pub fn detect_filesystem(path: &Path) -> Option<String> {
    #[cfg(target_os = "linux")]
    return fs_type_for_path_linux(path);
    #[cfg(target_os = "macos")]
    return fs_type_for_path_macos(path);
    #[cfg(windows)]
    return fs_type_for_path_windows(path);
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    let _ = path;
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    None
}

pub struct FsApplyReport {
    pub strategy: String,
    pub fs_type: String,
    pub reason: String,
    pub changes: Vec<String>,
    pub recommended_threads: Option<usize>,
    pub recommend_logical_only: bool,
}

pub struct FsApplyOutcome {
    pub recommended_threads: Option<usize>,
    pub recommend_logical_only: bool,
}

pub fn detect_and_apply(path: &Path, opt: &mut Options) -> Option<FsApplyReport> {
    // Allow opt-out
    if std::env::var("HYPERDU_FS_AUTO").ok().as_deref() == Some("0") {
        return None;
    }
    let fs = detect_filesystem(path).unwrap_or_else(|| "generic".into());
    Some(apply_filesystem(&fs, opt))
}

fn apply_filesystem(fs: &str, opt: &mut Options) -> FsApplyReport {
    let l = fs.to_ascii_lowercase();
    let looks_network = matches!(
        l.as_str(),
        "nfs" | "nfs4" | "cifs" | "smbfs" | "smb" | "remote" | "fuse.sshfs" | "9p" | "fuse"
    );
    let (strat, reason): (Box<dyn FileSystemStrategy>, String) = match l.as_str() {
        "ext4" => (Box::new(Ext4Strategy), "fstype=ext4".into()),
        "xfs" => (Box::new(XfsStrategy), "fstype=xfs".into()),
        "btrfs" => (Box::new(BtrfsStrategy), "fstype=btrfs".into()),
        "zfs" => (Box::new(ZfsStrategy), "fstype=zfs".into()),
        "drvfs" => (Box::new(DrvfsStrategy), "fstype=drvfs (WSL)".into()),
        "ntfs" => (
            Box::new(NativeStrategy("ntfs")),
            "native enumeration; MFT remains opt-in".into(),
        ),
        "refs" => (
            Box::new(NativeStrategy("refs")),
            "native enumeration; no NTFS raw parser".into(),
        ),
        "apfs" | "hfs" => (
            Box::new(NativeStrategy("darwin-bulk")),
            format!("fstype={l}"),
        ),
        _ if looks_network => (Box::new(NetworkStrategy), format!("network={l}")),
        _ => (Box::new(GenericStrategy), format!("fstype={l}")),
    };
    let mut changes = Vec::new();
    let outcome = strat.apply(opt, &mut changes);
    FsApplyReport {
        strategy: strat.name().into(),
        fs_type: l,
        reason,
        changes,
        recommended_threads: outcome.recommended_threads,
        recommend_logical_only: outcome.recommend_logical_only,
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn mount_match_requires_a_path_boundary() {
        assert!(path_under_mount("/mnt/f", "/mnt/f"));
        assert!(path_under_mount("/mnt/f/github", "/mnt/f"));
        // The case that misdetected: a sibling directory sharing a prefix.
        assert!(!path_under_mount("/mnt/foo", "/mnt/f"));
        assert!(!path_under_mount("/mnt/foo/bar", "/mnt/f"));
    }

    #[test]
    fn root_mount_matches_every_absolute_path() {
        assert!(path_under_mount("/", "/"));
        assert!(path_under_mount("/home/user", "/"));
    }

    #[test]
    fn octal_escapes_are_decoded_in_mount_points() {
        assert_eq!(unescape_mount_field(r"/mnt/my\040disk"), "/mnt/my disk");
        assert_eq!(unescape_mount_field(r"/mnt/a\011b"), "/mnt/a\tb");
        assert_eq!(unescape_mount_field("/plain/path"), "/plain/path");
        // A trailing backslash must not read past the end of the string.
        assert_eq!(unescape_mount_field("/trailing\\"), "/trailing\\");
    }
}

#[cfg(test)]
mod accounting_tests {
    use super::*;

    #[test]
    fn native_and_remote_strategy_selection_retains_semantics() {
        for (fs, strategy, threads) in [
            ("NTFS", "ntfs", None),
            ("ReFS", "refs", None),
            ("apfs", "darwin-bulk", None),
            ("hfs", "darwin-bulk", None),
            ("smbfs", "network", Some(4)),
            ("remote", "network", Some(4)),
            ("nfs", "network", Some(4)),
            ("unknown", "generic", None),
        ] {
            let mut opt = Options::default();
            let report = apply_filesystem(fs, &mut opt);
            assert_eq!(report.strategy, strategy, "{fs}");
            assert_eq!(report.recommended_threads, threads, "{fs}");
            assert!(opt.compute_physical, "{fs}");
            assert!(!opt.use_mft, "{fs} must not enable raw NTFS reads");
            assert!(!opt.count_hardlinks, "{fs}");
        }
    }

    #[cfg(any(windows, target_os = "macos", target_os = "linux"))]
    #[test]
    fn detects_a_real_temporary_directory() {
        let root = tempfile::tempdir().unwrap();
        let filesystem = detect_filesystem(root.path()).expect("temporary directory filesystem");
        assert!(!filesystem.is_empty());
        assert_ne!(filesystem, "generic");
        // A missing path must not be assigned an unrelated boot volume.
        #[cfg(any(windows, target_os = "macos"))]
        assert!(detect_filesystem(&root.path().join("missing")).is_none());
    }

    #[test]
    fn filesystem_tuning_preserves_requested_accounting() {
        let strategies: [&dyn FileSystemStrategy; 7] = [
            &GenericStrategy,
            &Ext4Strategy,
            &XfsStrategy,
            &BtrfsStrategy,
            &ZfsStrategy,
            &DrvfsStrategy,
            &NetworkStrategy,
        ];
        for strategy in strategies {
            for physical in [false, true] {
                let mut opt = Options {
                    compute_physical: physical,
                    ..Options::default()
                };
                let mut changes = Vec::new();
                strategy.apply(&mut opt, &mut changes);
                assert_eq!(opt.compute_physical, physical, "{}", strategy.name());
            }
        }
    }
}
