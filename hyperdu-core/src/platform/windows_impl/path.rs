//! UTF-16 path helpers for the Windows backends.
//!
//! Two forms of every path are needed:
//! * the *display* form (whatever the user passed, joined with `\`), used as
//!   the key in the result map so output matches the input spelling;
//! * the *open* form: absolute, normalized, `\\?\`-prefixed and NUL-terminated,
//!   so `CreateFileW`/`FindFirstFileExW` accept paths longer than `MAX_PATH`.

use std::{
    ffi::{OsStr, OsString},
    os::windows::ffi::{OsStrExt, OsStringExt},
    path::{Component, Path, PathBuf, Prefix},
    sync::OnceLock,
};

const SEP: u16 = b'\\' as u16;
const SLASH: u16 = b'/' as u16;
const COLON: u16 = b':' as u16;
const VERBATIM: [u16; 4] = [SEP, SEP, b'?' as u16, SEP];
const VERBATIM_UNC: [u16; 8] = [
    SEP,
    SEP,
    b'?' as u16,
    SEP,
    b'U' as u16,
    b'N' as u16,
    b'C' as u16,
    SEP,
];
const DEVICE_NS: [u16; 4] = [SEP, SEP, b'.' as u16, SEP];

fn cwd() -> &'static Path {
    static CWD: OnceLock<PathBuf> = OnceLock::new();
    CWD.get_or_init(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}

fn push_prefix(out: &mut Vec<u16>, kind: Prefix<'_>) {
    match kind {
        Prefix::Disk(d) | Prefix::VerbatimDisk(d) => {
            out.extend_from_slice(&VERBATIM);
            out.push(d as u16);
            out.push(b':' as u16);
        }
        Prefix::UNC(server, share) | Prefix::VerbatimUNC(server, share) => {
            out.extend_from_slice(&VERBATIM_UNC);
            out.extend(server.encode_wide());
            out.push(SEP);
            out.extend(share.encode_wide());
        }
        Prefix::Verbatim(x) => {
            out.extend_from_slice(&VERBATIM);
            out.extend(x.encode_wide());
        }
        Prefix::DeviceNS(x) => {
            out.extend_from_slice(&DEVICE_NS);
            out.extend(x.encode_wide());
        }
    }
}

fn raw_nul(p: &Path) -> Vec<u16> {
    let mut v: Vec<u16> = p.as_os_str().encode_wide().collect();
    v.push(0);
    v
}

/// Open form of `dir` (see module docs). Relative paths are resolved against
/// the process working directory once; `.`/`..` components are folded because
/// the verbatim prefix disables kernel-side normalization.
pub(super) fn to_wide_for_open(dir: &Path) -> Vec<u16> {
    let abs_storage;
    let p = if dir.is_absolute() {
        dir
    } else {
        abs_storage = cwd().join(dir);
        abs_storage.as_path()
    };
    let mut out: Vec<u16> = Vec::with_capacity(p.as_os_str().len() + 12);
    let mut normals: Vec<&OsStr> = Vec::new();
    let mut has_prefix = false;
    let mut has_root = false;
    for c in p.components() {
        match c {
            Component::Prefix(pc) => {
                push_prefix(&mut out, pc.kind());
                has_prefix = true;
            }
            Component::RootDir => has_root = true,
            Component::CurDir => {}
            Component::ParentDir => {
                normals.pop();
            }
            Component::Normal(n) => normals.push(n),
        }
    }
    if !(has_prefix && has_root) {
        // Drive-relative or otherwise unusual path: let the OS handle it.
        return raw_nul(dir);
    }
    out.push(SEP);
    for (i, n) in normals.iter().enumerate() {
        if i > 0 {
            out.push(SEP);
        }
        out.extend(n.encode_wide());
    }
    out.push(0);
    out
}

/// Builds child paths without re-encoding the parent for every entry.
pub(super) struct ChildPathBuilder {
    disp: Vec<u16>,
    disp_len: usize,
    open: Vec<u16>,
    open_len: usize,
}

impl ChildPathBuilder {
    pub fn new(dir: &Path) -> Self {
        let mut disp: Vec<u16> = dir.as_os_str().encode_wide().collect();
        // `C:` names the drive's current directory, and `Path::join` appends no
        // separator after a bare prefix. Inserting one here would silently
        // retarget every child at the drive root and break the parent chain.
        let bare_prefix = disp.last() == Some(&COLON);
        if !bare_prefix && !matches!(disp.last(), Some(&SEP) | Some(&SLASH)) {
            disp.push(SEP);
        }
        let mut open = to_wide_for_open(dir);
        open.pop(); // NUL
        if !bare_prefix && open.last() != Some(&SEP) {
            open.push(SEP);
        }
        disp.reserve(64);
        open.reserve(64);
        Self {
            disp_len: disp.len(),
            open_len: open.len(),
            disp,
            open,
        }
    }

    /// `dir.join(name)` with a single allocation (the result).
    pub fn path(&mut self, name: &[u16]) -> PathBuf {
        self.disp.truncate(self.disp_len);
        self.disp.extend_from_slice(name);
        PathBuf::from(OsString::from_wide(&self.disp))
    }

    /// NUL-terminated open form of the child; valid until the next call.
    pub fn wide_open(&mut self, name: &[u16]) -> &[u16] {
        self.open.truncate(self.open_len);
        self.open.extend_from_slice(name);
        self.open.push(0);
        &self.open
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(s: &str) -> Vec<u16> {
        let mut v: Vec<u16> = s.encode_utf16().collect();
        v.push(0);
        v
    }

    #[test]
    fn drive_paths_get_verbatim_prefix_and_backslashes() {
        assert_eq!(
            to_wide_for_open(Path::new("C:/Users/x")),
            w(r"\\?\C:\Users\x")
        );
        assert_eq!(
            to_wide_for_open(Path::new(r"C:\Users\x\")),
            w(r"\\?\C:\Users\x")
        );
        assert_eq!(to_wide_for_open(Path::new(r"C:\")), w(r"\\?\C:\"));
        assert_eq!(to_wide_for_open(Path::new(r"\\?\C:\a")), w(r"\\?\C:\a"));
    }

    #[test]
    fn dot_components_are_folded() {
        assert_eq!(
            to_wide_for_open(Path::new(r"C:\a\..\b\.\c")),
            w(r"\\?\C:\b\c")
        );
    }

    #[test]
    fn unc_paths_use_unc_prefix() {
        assert_eq!(
            to_wide_for_open(Path::new(r"\\server\share\dir")),
            w(r"\\?\UNC\server\share\dir")
        );
    }

    #[test]
    fn relative_paths_are_resolved_against_cwd() {
        let got = to_wide_for_open(Path::new("."));
        let expect = to_wide_for_open(&std::env::current_dir().unwrap());
        assert_eq!(got, expect);
    }

    #[test]
    fn drive_relative_children_match_path_join() {
        // `C:` means "the current directory on C:", not "the root of C:".
        let dir = Path::new("C:");
        let mut b = ChildPathBuilder::new(dir);
        let name: Vec<u16> = "child".encode_utf16().collect();
        assert_eq!(b.path(&name), dir.join("child"));
        assert_eq!(b.path(&name), Path::new("C:child"));
        assert_eq!(b.wide_open(&name), &w("C:child")[..]);
    }

    #[test]
    fn child_builder_matches_path_join() {
        let dir = Path::new("C:/root");
        let mut b = ChildPathBuilder::new(dir);
        let name: Vec<u16> = "child".encode_utf16().collect();
        assert_eq!(b.path(&name), dir.join("child"));
        let other: Vec<u16> = "x".encode_utf16().collect();
        assert_eq!(b.path(&other), dir.join("x"));
        assert_eq!(b.wide_open(&other), &w(r"\\?\C:\root\x")[..]);
    }
}
