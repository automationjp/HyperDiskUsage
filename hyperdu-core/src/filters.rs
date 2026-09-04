use std::path::Path;

use crate::Options;

#[inline(always)]
pub fn path_excluded(p: &Path, opt: &Options) -> bool {
    if let Some(gs) = &opt.exclude_glob_set {
        if gs.is_match(p) {
            return true;
        }
    }
    if let Some(rs) = &opt.exclude_regex_set {
        if rs.is_match(p.to_string_lossy().as_ref()) {
            return true;
        }
    }
    should_exclude_legacy(p, &opt.exclude_contains)
}

/// Whether a directory entry is excluded, using the same rule the scan backends
/// apply: match the entry's own name unless a filter genuinely needs the full
/// path. Matching the full path would also exclude everything under a root whose
/// own path happens to contain a pattern.
#[inline]
pub fn entry_excluded(p: &Path, opt: &Options) -> bool {
    if opt.needs_path_filter {
        return path_excluded(p, opt);
    }
    let Some(name) = p.file_name() else {
        return false;
    };
    let name = name.to_string_lossy();
    opt.exclude_contains
        .iter()
        .any(|q| !q.is_empty() && name.contains(q.as_str()))
}

#[inline(always)]
fn should_exclude_legacy(p: &Path, patterns: &[String]) -> bool {
    if patterns.is_empty() {
        return false;
    }
    let s = p.as_os_str().to_string_lossy();
    patterns.iter().any(|q| !q.is_empty() && s.contains(q))
}

// name_* helpers live in crate root for cross-module reuse (see lib.rs)
