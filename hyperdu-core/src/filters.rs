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

#[inline(always)]
fn should_exclude_legacy(p: &Path, patterns: &[String]) -> bool {
    if patterns.is_empty() {
        return false;
    }
    let s = p.as_os_str().to_string_lossy();
    patterns.iter().any(|q| !q.is_empty() && s.contains(q))
}

// name_* helpers live in crate root for cross-module reuse

#[cfg(windows)]
#[inline(always)]
pub fn wname_contains_patterns_lossy(name: &std::ffi::OsString, patterns: &[String]) -> bool {
    if patterns.is_empty() {
        return false;
    }
    let s = name.to_string_lossy();
    patterns.iter().any(|q| !q.is_empty() && s.contains(q))
}
