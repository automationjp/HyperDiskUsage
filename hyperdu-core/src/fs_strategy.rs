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
            disable_uring: false,
            recommend_logical_only: false,
        }
    }
}

struct Ext4Strategy;
impl FileSystemStrategy for Ext4Strategy {
    fn name(&self) -> &'static str {
        "ext4"
    }
    fn apply(&self, _opt: &mut Options, report: &mut Vec<String>) -> FsApplyOutcome {
        // Favor larger dirent buffer on fast storage
        std::env::set_var("HYPERDU_GETDENTS_BUF_KB", "128");
        report.push("getdents_buf_kb=128".into());
        // Enable prefetch hints if compiled
        std::env::set_var("HYPERDU_PREFETCH", "1");
        report.push("prefetch=1".into());
        FsApplyOutcome {
            recommended_threads: None,
            disable_uring: false,
            recommend_logical_only: false,
        }
    }
}

struct XfsStrategy;
impl FileSystemStrategy for XfsStrategy {
    fn name(&self) -> &'static str {
        "xfs"
    }
    fn apply(&self, _opt: &mut Options, report: &mut Vec<String>) -> FsApplyOutcome {
        std::env::set_var("HYPERDU_GETDENTS_BUF_KB", "128");
        report.push("getdents_buf_kb=128".into());
        std::env::set_var("HYPERDU_PREFETCH", "1");
        report.push("prefetch=1".into());
        FsApplyOutcome {
            recommended_threads: None,
            disable_uring: false,
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
        // On CoW/comp-possible FS, logical size is often cheaper; keep physical but avoid aggressive blocks path
        // Switch to logical-only by default for better responsiveness
        opt.compute_physical = false;
        report.push("compute_physical=false".into());
        std::env::set_var("HYPERDU_GETDENTS_BUF_KB", "128");
        report.push("getdents_buf_kb=128".into());
        // Do not enable prefetch by default
        std::env::set_var("HYPERDU_PREFETCH", "0");
        report.push("prefetch=0".into());
        let _ = opt; // placeholder for future
        FsApplyOutcome {
            recommended_threads: None,
            disable_uring: false,
            recommend_logical_only: true,
        }
    }
}

struct ZfsStrategy;
impl FileSystemStrategy for ZfsStrategy {
    fn name(&self) -> &'static str {
        "zfs"
    }
    fn apply(&self, _opt: &mut Options, report: &mut Vec<String>) -> FsApplyOutcome {
        std::env::set_var("HYPERDU_GETDENTS_BUF_KB", "128");
        report.push("getdents_buf_kb=128".into());
        std::env::set_var("HYPERDU_PREFETCH", "1");
        report.push("prefetch=1".into());
        FsApplyOutcome {
            recommended_threads: None,
            disable_uring: false,
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
        // WSL DrvFS: slow statx/blocks; avoid physical size and reduce parallel pressure
        opt.compute_physical = false;
        report.push("compute_physical=false".into());
        // Slightly smaller buffer (context switch heavy)
        std::env::set_var("HYPERDU_GETDENTS_BUF_KB", "64");
        report.push("getdents_buf_kb=64".into());
        // Disable prefetch hints
        std::env::set_var("HYPERDU_PREFETCH", "0");
        report.push("prefetch=0".into());
        // Suggest fewer threads and disable uring
        FsApplyOutcome {
            recommended_threads: Some(4),
            disable_uring: true,
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
        // Network FS: prefer logical sizes, limit pressure
        opt.compute_physical = false;
        report.push("compute_physical=false".into());
        std::env::set_var("HYPERDU_GETDENTS_BUF_KB", "64");
        report.push("getdents_buf_kb=64".into());
        std::env::set_var("HYPERDU_PREFETCH", "0");
        report.push("prefetch=0".into());
        // Optionally reduce threads in caller if needed (not adjusted here)
        FsApplyOutcome {
            recommended_threads: Some(4),
            disable_uring: true,
            recommend_logical_only: false,
        }
    }
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
                            let mp = pre_parts[4];
                            if path.to_string_lossy().starts_with(mp) {
                                let post_parts: Vec<&str> = post[3..].split_whitespace().collect();
                                if post_parts.len() >= 1 {
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
                        let mp = parts[1];
                        let fs = parts[2];
                        if path.to_string_lossy().starts_with(mp) {
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

#[cfg(not(target_os = "linux"))]
fn fs_type_for_path_linux(_p: &Path) -> Option<String> {
    None
}

pub struct FsApplyReport {
    pub strategy: String,
    pub fs_type: String,
    pub reason: String,
    pub changes: Vec<String>,
    pub recommended_threads: Option<usize>,
    pub disable_uring: bool,
    pub recommend_logical_only: bool,
}

pub struct FsApplyOutcome {
    pub recommended_threads: Option<usize>,
    pub disable_uring: bool,
    pub recommend_logical_only: bool,
}

pub fn detect_and_apply(path: &Path, opt: &mut Options) -> Option<FsApplyReport> {
    // Allow opt-out
    if std::env::var("HYPERDU_FS_AUTO").ok().as_deref() == Some("0") {
        return None;
    }
    let fs = fs_type_for_path_linux(path).unwrap_or_else(|| "generic".into());
    let l = fs.to_ascii_lowercase();
    let looks_network = matches!(
        l.as_str(),
        "nfs" | "nfs4" | "cifs" | "smbfs" | "fuse.sshfs" | "9p" | "fuse"
    );
    let (strat, reason): (Box<dyn FileSystemStrategy>, String) = match l.as_str() {
        "ext4" => (Box::new(Ext4Strategy), "fstype=ext4".into()),
        "xfs" => (Box::new(XfsStrategy), "fstype=xfs".into()),
        "btrfs" => (Box::new(BtrfsStrategy), "fstype=btrfs".into()),
        "zfs" => (Box::new(ZfsStrategy), "fstype=zfs".into()),
        "drvfs" => (Box::new(DrvfsStrategy), "fstype=drvfs (WSL)".into()),
        _ if looks_network => (Box::new(NetworkStrategy), format!("network={}", l)),
        _ => (Box::new(GenericStrategy), format!("fstype={}", l)),
    };
    let mut changes = Vec::new();
    let outcome = strat.apply(opt, &mut changes);
    Some(FsApplyReport {
        strategy: strat.name().into(),
        fs_type: l,
        reason,
        changes,
        recommended_threads: outcome.recommended_threads,
        disable_uring: outcome.disable_uring,
        recommend_logical_only: outcome.recommend_logical_only,
    })
}
