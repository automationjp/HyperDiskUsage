//! Removed backend switches must fail instead of silently accepting ineffective tuning.
use std::process::Command;

const REMOVED_OPTIONS: &[&[&str]] = &[
    &["--uring-batch", "8"],
    &["--uring-depth", "8"],
    &["--no-uring"],
    &["--uring-sqpoll"],
    &["--uring-sqpoll-idle-ms", "10"],
    &["--uring-sqpoll-cpu", "0"],
    &["--uring-coop"],
    &["--getdents-buf-kb", "64"],
];

#[test]
fn removed_backend_options_are_not_advertised() {
    let output = Command::new(env!("CARGO_BIN_EXE_hyperdu"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let help = String::from_utf8(output.stdout).unwrap();
    for option in REMOVED_OPTIONS {
        assert!(
            !help.contains(option[0]),
            "obsolete option in help: {option:?}"
        );
    }
}

#[test]
fn removed_backend_options_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    for option in REMOVED_OPTIONS {
        let output = Command::new(env!("CARGO_BIN_EXE_hyperdu"))
            .args(*option)
            .arg(dir.path())
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(2), "{option:?}: {output:?}");
        assert!(
            stderr.contains("unexpected argument"),
            "{option:?}: {stderr}"
        );
        assert!(stderr.contains(option[0]), "{option:?}: {stderr}");
    }
}

#[test]
fn scan_without_removed_options_still_reports_exact_logical_size() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("sample.bin"), [1u8; 123]).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_hyperdu"))
        .args([
            "--compat",
            "gnu",
            "--logical-only",
            "--block-size",
            "1",
            "--threads",
            "1",
        ])
        .arg(dir.path())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(stdout.lines().count(), 1, "{stdout}");
    assert_eq!(stdout.split_whitespace().next(), Some("123"), "{stdout}");
}

fn conditional_options() -> Vec<(&'static [&'static str], bool)> {
    vec![
        (
            &["--approximate"],
            cfg!(all(unix, not(target_os = "macos"))),
        ),
        (
            &["--dir-yield-every", "10"],
            cfg!(all(unix, not(target_os = "macos"))),
        ),
        (
            &["--prefetch=false"],
            cfg!(all(target_os = "linux", target_arch = "x86_64")),
        ),
        (&["--pin-threads"], cfg!(target_os = "linux")),
        (&["--galb-buf-kb", "64"], cfg!(target_os = "macos")),
        (&["--win-ntquery"], cfg!(all(windows, target_env = "msvc"))),
        (&["--mft"], cfg!(all(windows, target_env = "msvc"))),
        (&["--time"], cfg!(feature = "time-format")),
        (&["--time-kind", "mtime"], cfg!(feature = "time-format")),
        (&["--time-style", "iso"], cfg!(feature = "time-format")),
    ]
}

#[test]
fn help_only_advertises_options_available_in_this_build() {
    let output = Command::new(env!("CARGO_BIN_EXE_hyperdu"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let help = String::from_utf8(output.stdout).unwrap();
    for (args, supported) in conditional_options() {
        let name = args[0].split('=').next().unwrap();
        let advertised = help.lines().any(|line| line.trim_start().starts_with(name));
        assert_eq!(advertised, supported, "{name}: {help}");
    }
}

#[test]
fn platform_and_build_options_are_rejected_or_run_on_a_small_fixture() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("sample.bin"), [1u8; 123]).unwrap();
    for (args, supported) in conditional_options() {
        let output = Command::new(env!("CARGO_BIN_EXE_hyperdu"))
            .args(["--compat", "gnu", "--logical-only", "--threads", "1"])
            .args(args)
            .arg(dir.path())
            .output()
            .unwrap();
        if supported {
            assert!(output.status.success(), "{args:?}: {output:?}");
        } else {
            let name = args[0].split('=').next().unwrap();
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert_eq!(output.status.code(), Some(2), "{args:?}: {output:?}");
            assert!(
                stderr.contains("unexpected argument") && stderr.contains(name),
                "{stderr}"
            );
        }
    }
}
