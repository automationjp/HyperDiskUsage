//! `--max-depth` must mean what GNU du means by it: limit which rows are
//! printed, not how deep the scan goes.
//!
//! The regression these cover: `--max-depth` was wired straight into the
//! scanner's traversal cut, so every printed total silently dropped everything
//! below the cut. `hyperdu C:/ --max-depth 2` reported 74 GiB for a volume
//! holding 894 GiB, in the same output block that printed the correct
//! `used=928 GiB` from the filesystem -- and it did it under `--compat gnu`,
//! which promises du's numbers.
//!
//! The traversal cut is still worth having, so it keeps its own flag
//! (`--prune-depth`) where truncated totals are the point rather than a
//! surprise.

use std::{path::Path, process::Command};

fn bin_path() -> &'static str {
    env!("CARGO_BIN_EXE_hyperdu")
}

/// root.bin 5_000 | a/a.bin 50_000 | a/b/b.bin 500_000
///
/// Sizes are distinct orders of magnitude so a truncated total cannot
/// accidentally equal the correct one.
fn make_tree() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("a/b")).unwrap();
    std::fs::write(root.join("root.bin"), vec![0u8; 5_000]).unwrap();
    std::fs::write(root.join("a/a.bin"), vec![0u8; 50_000]).unwrap();
    std::fs::write(root.join("a/b/b.bin"), vec![0u8; 500_000]).unwrap();
    tmp
}

/// `-b` is `--apparent-size --block-size=1`, which takes allocation granularity
/// out of the comparison: the expected numbers are then exactly the bytes
/// written above.
fn du_rows(root: &Path, extra: &[&str]) -> Vec<(u64, String)> {
    du_rows_for_size(root, extra, &["-b"])
}

fn du_rows_for_size(root: &Path, extra: &[&str], size_args: &[&str]) -> Vec<(u64, String)> {
    let out = Command::new(bin_path())
        .arg(root)
        .args(["--compat", "gnu"])
        .args(size_args)
        .args(extra)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "cli failed: status={:?} stderr={}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| {
            let (size, path) = l
                .split_once('\t')
                .unwrap_or_else(|| panic!("expected `size<TAB>path`, got {l:?}"));
            (size.parse().unwrap(), path.to_string())
        })
        .collect()
}

#[test]
fn max_depth_limits_printed_rows_but_leaves_totals_whole() {
    let tmp = make_tree();
    let rows = du_rows(tmp.path(), &["--max-depth", "1"]);

    assert_eq!(
        rows.len(),
        2,
        "du -d 1 prints the operand and its children, got: {rows:?}"
    );
    // Post-order: the child row first, the operand row last.
    assert_eq!(
        rows[0].0, 550_000,
        "the a row must include a/b/b.bin even though a/b is not printed"
    );
    assert_eq!(
        rows[1].0, 555_000,
        "operand total must include a/b/b.bin even though a/b is not printed"
    );
}

/// `du -d 0` is `--summarize`. Verified against du (GNU coreutils) 8.32:
/// `du -b -d 0 X` and `du -b -s X` produce byte-identical stdout.
#[test]
fn max_depth_zero_prints_only_the_operand_row() {
    let tmp = make_tree();
    let rows = du_rows(tmp.path(), &["--max-depth", "0"]);

    assert_eq!(rows.len(), 1, "one row per operand, got: {rows:?}");
    assert_eq!(rows[0].0, 555_000, "and it carries the complete total");
}

/// du has no token for "unlimited" -- it is the absence of the flag. Any
/// in-band sentinel would collide with a value du reads as a real depth.
#[test]
fn omitting_the_flag_is_unlimited() {
    let tmp = make_tree();
    let rows = du_rows(tmp.path(), &[]);
    assert_eq!(rows.len(), 3, "root, a, a/b -- got: {rows:?}");
}

#[test]
fn max_depth_beyond_the_tree_changes_nothing() {
    let tmp = make_tree();
    assert_eq!(
        du_rows(tmp.path(), &["--max-depth", "9"]),
        du_rows(tmp.path(), &[]),
    );
}

#[test]
fn the_short_form_d_is_accepted_like_du() {
    let tmp = make_tree();
    assert_eq!(
        du_rows(tmp.path(), &["-d", "1"]),
        du_rows(tmp.path(), &["--max-depth", "1"]),
    );
}

/// du parses the value with `xstrtoumax` base 0, so `010` is octal 8 -- deep
/// enough to cover this whole fixture -- while plain decimal `10` happens to
/// mean the same here. The case that distinguishes them is `08`, which is not
/// a valid octal literal and which du rejects; see the error test below.
#[test]
fn the_value_is_parsed_in_base_zero_like_du() {
    let tmp = make_tree();
    let unlimited = du_rows(tmp.path(), &[]);
    assert_eq!(du_rows(tmp.path(), &["-d", "010"]), unlimited);
    assert_eq!(du_rows(tmp.path(), &["-d", "0x2"]), unlimited);
    assert_eq!(
        du_rows(tmp.path(), &["-d", "0x1"]),
        du_rows(tmp.path(), &["-d", "1"]),
        "0x1 is depth 1"
    );
    assert_eq!(du_rows(tmp.path(), &["-d", " 2"]), unlimited);
    assert_eq!(du_rows(tmp.path(), &["-d", "+2"]), unlimited);
}

/// A value du accepts must not be capped by our narrower integer type.
#[test]
fn values_above_u32_are_accepted_like_du() {
    let tmp = make_tree();
    let unlimited = du_rows(tmp.path(), &[]);
    assert_eq!(du_rows(tmp.path(), &["-d", "4294967296"]), unlimited);
    assert_eq!(
        du_rows(tmp.path(), &["-d", "18446744073709551615"]),
        unlimited
    );
}

/// du's own diagnostic, down to the exit status: 1, not clap's 2, because a
/// script branching on the status has to see what du would give it.
#[test]
fn an_invalid_depth_fails_the_way_du_fails() {
    let tmp = make_tree();
    for bad in [
        "-1",
        "abc",
        "",
        "1.5",
        "2abc",
        "1 ",
        "1e3",
        "08",
        "1K",
        "18446744073709551616",
    ] {
        let out = Command::new(bin_path())
            .arg(tmp.path())
            .args(["--compat", "gnu", "-b", "-d", bad])
            .output()
            .unwrap();

        assert_eq!(
            out.status.code(),
            Some(1),
            "exit status for -d {bad:?}, stderr={}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            out.stdout.is_empty(),
            "a rejected depth must not also produce output, got {:?}",
            String::from_utf8_lossy(&out.stdout)
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("invalid maximum depth") && stderr.contains(bad),
            "du-shaped diagnostic naming the value, got: {stderr:?}"
        );
        assert!(
            stderr.contains("--help' for more information."),
            "du follows the error with the help hint, got: {stderr:?}"
        );
    }
}

/// The depth error is not a special case: every usage error exits 1. Without
/// this, `--max-depth=abc` and `--threads=abc` reported different statuses from
/// the same binary.
#[test]
fn other_usage_errors_exit_one_too() {
    let tmp = make_tree();
    for args in [
        vec!["--no-such-flag"],
        vec!["--threads", "abc"],
        vec!["--top", "abc"],
    ] {
        let out = Command::new(bin_path())
            .arg(tmp.path())
            .args(&args)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(1), "{args:?}: {out:?}");
    }
}

/// `--help` and `--version` are not errors.
#[test]
fn help_and_version_still_exit_zero() {
    for flag in ["--help", "--version"] {
        let out = Command::new(bin_path()).arg(flag).output().unwrap();
        assert_eq!(out.status.code(), Some(0), "{flag}: {out:?}");
    }
}

/// A typo used to be answered with 1 KiB blocks and a status of 0 -- numbers
/// that looked right and were in the wrong unit.
#[test]
fn an_invalid_block_size_fails_instead_of_defaulting() {
    let tmp = make_tree();
    for bad in ["xyz", "0", "1KX", ""] {
        let out = Command::new(bin_path())
            .arg(tmp.path())
            .args(["--compat", "gnu", "--block-size", bad])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(1), "--block-size={bad:?}: {out:?}");
        assert!(
            out.stdout.is_empty(),
            "--block-size={bad:?} must not also print sizes"
        );
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("invalid --block-size argument"),
            "--block-size={bad:?}: {out:?}"
        );
    }
}

/// du writes nothing to stderr on a successful run. The tuning banner used to
/// go out on every single invocation, compat mode or not.
#[test]
fn a_clean_compat_run_says_nothing_on_stderr() {
    let tmp = make_tree();
    let out = Command::new(bin_path())
        .arg(tmp.path())
        .args(["--compat", "gnu", "-b"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(
        out.stderr.is_empty(),
        "expected empty stderr, got: {:?}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// ...but HyperDU's own mode is still allowed to narrate.
#[test]
fn the_native_mode_keeps_its_diagnostics() {
    let tmp = make_tree();
    let out = Command::new(bin_path())
        .arg(tmp.path())
        .arg("--logical-only")
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("fs-auto:"),
        "native mode should still report its tuning, got: {:?}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// GNU takes the last occurrence silently. uutils regressed into an error
/// here, which is exactly the shape a clap-based rewrite falls into.
#[test]
fn a_repeated_depth_flag_takes_the_last_one() {
    let tmp = make_tree();
    assert_eq!(
        du_rows(tmp.path(), &["-d", "9", "-d", "1"]),
        du_rows(tmp.path(), &["-d", "1"]),
    );
    assert_eq!(
        du_rows(tmp.path(), &["-d", "1", "-d", "9"]),
        du_rows(tmp.path(), &["-d", "9"]),
    );
}

/// du lays a tree out depth-first in post-order: every child row before its
/// parent, the operand row last. `du <dir> | tail -1` depends on it.
///
/// The fixture is a trap for the obvious shortcut: `!` (0x21) and `.` (0x2E)
/// both sort below `/` (0x2F), so ordering by raw path bytes -- forward or
/// reversed -- interleaves `a!.txt`/`a.txt` with the contents of the sibling
/// directory `a`. Only a component-wise comparison gets this right.
#[test]
fn rows_come_out_depth_first_with_the_operand_last() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    for dir in ["a", "a/deep", "ab", "b"] {
        std::fs::create_dir_all(root.join(dir)).unwrap();
    }
    for f in ["a!.txt", "a.txt", "a/x", "a/deep/y", "ab/z", "b/w"] {
        std::fs::write(root.join(f), vec![0u8; 10]).unwrap();
    }

    let rows = du_rows(root, &[]);
    let paths: Vec<&str> = rows.iter().map(|(_, p)| p.as_str()).collect();
    let root_s = root.to_string_lossy().into_owned();

    assert_eq!(
        paths.last().copied(),
        Some(root_s.as_str()),
        "the operand row is last, got: {paths:?}"
    );

    // Every row must precede all of its ancestors.
    for (i, child) in paths.iter().enumerate() {
        for (j, parent) in paths.iter().enumerate() {
            if i == j {
                continue;
            }
            let nested = std::path::Path::new(child)
                .strip_prefix(std::path::Path::new(parent))
                .is_ok();
            assert!(
                !nested || i < j,
                "{child} is inside {parent} but printed after it: {paths:?}"
            );
        }
    }

    // Siblings stay in a deterministic order, and a subtree stays contiguous:
    // everything under `a` comes out before `ab` starts.
    // Matched on the final component, not a suffix: `ends_with("b")` also
    // matches `ab`.
    let pos = |name: &str| {
        paths
            .iter()
            .position(|p| {
                std::path::Path::new(p)
                    .file_name()
                    .is_some_and(|n| n == name)
            })
            .unwrap_or_else(|| panic!("{name} missing from {paths:?}"))
    };
    assert!(pos("deep") < pos("a"), "a/deep precedes a: {paths:?}");
    assert!(pos("a") < pos("ab"), "the a subtree precedes ab: {paths:?}");
    assert!(pos("ab") < pos("b"), "ab precedes b: {paths:?}");
}

/// Running twice gives the same order. du's does not -- its sibling order is
/// the filesystem's enumeration order -- which is exactly why HyperDU sorts.
#[test]
fn the_order_is_deterministic() {
    let tmp = make_tree();
    assert_eq!(du_rows(tmp.path(), &[]), du_rows(tmp.path(), &[]));
}

/// The leftmost operand owns a row, because du walks operands left to right
/// and skips what it has already counted. Taking the closest operand instead
/// leaked `a/b` into a `-d 1` listing.
#[test]
fn nested_operands_measure_from_the_leftmost_one() {
    let tmp = make_tree();
    let out = Command::new(bin_path())
        .arg(tmp.path())
        .arg(tmp.path().join("a"))
        .args(["--compat", "gnu", "-b", "-d", "1"])
        .output()
        .unwrap();
    assert!(out.status.success());

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains(&format!("{}", tmp.path().join("a").join("b").display())),
        "a/b is two levels below the owning operand, got: {stdout}"
    );
}

#[test]
fn prune_depth_keeps_the_traversal_cut_and_its_truncated_totals() {
    let tmp = make_tree();
    let rows = du_rows(tmp.path(), &["--prune-depth", "1"]);

    assert_eq!(rows.len(), 2, "nothing below the cut is scanned: {rows:?}");
    assert_eq!(rows[0].0, 50_000);
    assert_eq!(
        rows[1].0, 55_000,
        "pruning is supposed to drop a/b from the total -- that is what it is for"
    );
}

/// Truncated totals are defensible only when the caller cannot miss that they
/// are truncated, so the cut announces itself. stderr, not stdout: du-shaped
/// stdout has to stay parsable.
#[test]
fn prune_depth_warns_that_totals_are_partial() {
    let tmp = make_tree();
    let out = Command::new(bin_path())
        .arg(tmp.path())
        .args(["--compat", "gnu", "-b", "--prune-depth", "1"])
        .output()
        .unwrap();
    assert!(out.status.success());

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--prune-depth"),
        "the warning should name the flag that caused it, got: {stderr:?}"
    );
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains("prune-depth"),
        "the warning must not land in du-compatible stdout"
    );
}

/// The two flags answer different questions, so asking both at once is a
/// contradiction worth rejecting rather than silently resolving.
#[test]
fn max_depth_and_prune_depth_together_are_rejected() {
    let tmp = make_tree();
    let out = Command::new(bin_path())
        .arg(tmp.path())
        .args(["--max-depth", "1", "--prune-depth", "1"])
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "expected the CLI to refuse both at once"
    );
}

/// The HyperDU-native output carries the same promise: its `Total:` line sits
/// directly above a `Disk: used=...` line read from the filesystem, so a
/// truncated total reads as a contradiction.
#[test]
fn hyperdu_output_total_is_whole_under_max_depth() {
    let tmp = make_tree();
    for mode in [&[][..], &["--logical-only"][..]] {
        let total = native_total(tmp.path(), mode);
        assert!(total.contains("files=3"), "{total}");
        assert!(total.contains("log=541.99 KiB"), "{total}");
        for depth in ["0", "1"] {
            let mut args = mode.to_vec();
            args.extend(["--max-depth", depth]);
            assert_eq!(
                native_total(tmp.path(), &args),
                total,
                "depth {depth} must preserve logical/physical totals and counts"
            );
        }
    }
}

fn native_total(root: &Path, args: &[&str]) -> String {
    let out = Command::new(bin_path())
        .arg(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    stdout
        .lines()
        .find(|l| l.trim_start().starts_with("Total:"))
        .unwrap_or_else(|| panic!("no Total line in: {stdout}"))
        .to_owned()
}

#[test]
fn display_depth_preserves_each_ancestor_in_both_size_modes() {
    let tmp = make_tree();
    for size_args in [&["-b"][..], &["--block-size", "1"][..]] {
        let unlimited = du_rows_for_size(tmp.path(), &[], size_args);
        assert_eq!(unlimited.len(), 3);
        assert!(
            unlimited[0].0 > 0,
            "deep file must contribute: {unlimited:?}"
        );
        for (depth, expected_rows) in [("0", 1), ("1", 2)] {
            let limited = du_rows_for_size(tmp.path(), &["--max-depth", depth], size_args);
            assert_eq!(limited.len(), expected_rows);
            assert_eq!(
                limited,
                unlimited[unlimited.len() - expected_rows..],
                "only rows may disappear; all retained capacities must be unchanged"
            );
        }
    }
}
