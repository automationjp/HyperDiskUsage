//! Direct command-to-command parity on GNU/Linux; no external byte adjustment.
//! AWS acceptance also runs this suite. Local Linux runs are correctness only.
#![cfg(all(target_os = "linux", target_arch = "x86_64", not(target_env = "musl")))]

use std::{
    collections::BTreeMap,
    ffi::CString,
    fs,
    os::unix::{ffi::OsStrExt, fs::symlink, net::UnixListener},
    path::{Path, PathBuf},
    process::Command,
};

fn gnu_du() -> PathBuf {
    let binary = std::env::var_os("HYPERDU_GNU_DU")
        .map(PathBuf::from)
        .unwrap_or_else(|| "du".into());
    let version = Command::new(&binary).arg("--version").output();
    let available = version.as_ref().is_ok_and(|v| {
        v.status.success() && String::from_utf8_lossy(&v.stdout).contains("GNU coreutils")
    });
    assert!(
        available,
        "GNU du is required for this Linux parity suite; set HYPERDU_GNU_DU to its executable"
    );
    binary
}

fn directory_rows(command: &mut Command) -> BTreeMap<PathBuf, u64> {
    let output = command
        .env("LC_ALL", "C")
        .env_remove("POSIXLY_CORRECT")
        .env_remove("BLOCK_SIZE")
        .env_remove("BLOCKSIZE")
        .env_remove("DU_BLOCK_SIZE")
        .output()
        .expect("run command");
    assert!(
        output.status.success(),
        "{command:?}: status={} stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).unwrap();
    let mut rows = BTreeMap::new();
    for line in text.lines() {
        let (bytes, path) = line.split_once('\t').expect("bytes<TAB>path");
        assert!(
            rows.insert(PathBuf::from(path), bytes.parse().expect("integer bytes"))
                .is_none(),
            "duplicate directory row: {path}"
        );
    }
    rows
}

fn hyperdu(root: &Path, threads: usize, split: usize, apparent: bool, compat: &str) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_hyperdu"));
    command
        .args([
            "--compat",
            compat,
            "--block-size",
            "1",
            "--one-file-system",
            "--no-fs-auto",
        ])
        .args([
            "--threads",
            &threads.to_string(),
            "--dir-yield-every",
            &split.to_string(),
        ]);
    if apparent {
        command.arg("--apparent-size");
    }
    command.arg("--").arg(root);
    command
}

fn reference(du: &Path, root: &Path, apparent: bool) -> BTreeMap<PathBuf, u64> {
    let mut command = Command::new(du);
    command.args(["-x", "--block-size=1"]);
    if apparent {
        command.arg("--apparent-size");
    }
    command.arg("--").arg(root);
    directory_rows(&mut command)
}

#[test]
fn strict_counts_empty_directory_allocation_like_gnu_du() {
    let du = gnu_du();
    let fixture = tempfile::tempdir().unwrap();
    fs::create_dir(fixture.path().join("empty")).unwrap();
    for apparent in [false, true] {
        for compat in ["gnu-strict", "posix-strict"] {
            assert_eq!(
                directory_rows(&mut hyperdu(fixture.path(), 1, 0, apparent, compat)),
                reference(&du, fixture.path(), apparent)
            );
        }
    }
}

#[test]
fn strict_matches_gnu_du_without_correcting_file_or_directory_bytes() {
    let du = gnu_du();
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path();
    let nested = root.join("nested");
    fs::create_dir(&nested).unwrap();
    fs::create_dir(root.join("empty")).unwrap();
    fs::write(nested.join("regular"), vec![7u8; 8193]).unwrap();
    fs::hard_link(nested.join("regular"), nested.join("hardlink")).unwrap();
    fs::File::create(nested.join("sparse"))
        .unwrap()
        .set_len(8 * 1024 * 1024)
        .unwrap();
    symlink("regular", nested.join("short-link")).unwrap();
    symlink("x".repeat(1600), nested.join("long-dangling-link")).unwrap();
    fs::hard_link(
        nested.join("long-dangling-link"),
        nested.join("hardlinked-symlink"),
    )
    .unwrap();
    symlink("nested", root.join("directory-link")).unwrap();
    let fifo = CString::new(nested.join("fifo").as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    let _socket = UnixListener::bind(nested.join("socket")).unwrap();
    for n in 0..100 {
        fs::write(nested.join(format!("file-{n:03}")), [n as u8; 23]).unwrap();
    }
    for apparent in [false, true] {
        let expected = reference(&du, root, apparent);
        for (threads, split) in [(1, 0), (4, 0), (1, 8), (4, 8)] {
            for compat in ["gnu-strict", "posix-strict"] {
                let actual = directory_rows(&mut hyperdu(root, threads, split, apparent, compat));
                assert_eq!(
                    actual, expected,
                    "threads={threads}, split={split}, apparent={apparent}, compat={compat}"
                );
            }
        }
    }
}
