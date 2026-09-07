//! `--tune-only`: probe a few option combinations and report the fastest.
//!
//! Lifted verbatim out of `main`, which had grown to 1,049 lines against a
//! project rule of 50. This block was the largest self-contained piece: it runs
//! only when `--tune-only` is passed and always returns, so nothing after it in
//! `main` depended on anything it computed.

use std::path::PathBuf;

use anyhow::Result;
use hyperdu_core::Options;

use crate::Args;

/// Time a handful of candidate settings and print the fastest, then stop.
///
/// Takes both by reference: the probe clones `opt` per candidate and never
/// writes back, so a caller's options are untouched.
pub(crate) fn run_probe(args: &Args, opt: &Options) -> Result<()> {
    let secs = if args.tune_secs <= 0.1 {
        2.0
    } else {
        args.tune_secs
    };
    let mut probe = opt.clone();
    if probe.max_depth == 0 {
        probe.max_depth = 1;
    }
    probe.compute_physical = false;
    probe.progress_every = 0;
    let candidates: [usize; 6] = [8192, 16384, 32768, 65536, 131072, 262144];
    let root_probe = args
        .roots
        .first()
        .cloned()
        .unwrap_or_else(|| PathBuf::from("."));

    let measure = |yield_every: usize, log: bool| -> Option<(f64, u64, f64)> {
        probe
            .dir_yield_every
            .store(yield_every, std::sync::atomic::Ordering::Relaxed);
        let ts = std::time::Instant::now();
        let map = hyperdu_core::scan_directory(&root_probe, &probe).ok()?;
        let dt = ts.elapsed().as_secs_f64().max(1e-6);
        let total = *map
            .get(&root_probe)
            .unwrap_or(&hyperdu_core::Stat::default());
        let rate = (total.files as f64) / dt;
        if log {
            println!(
                "tune: yield={} -> {:.0} files/s (files={})",
                yield_every, rate, total.files
            );
        }
        Some((rate, total.files, dt))
    };

    let t_start = std::time::Instant::now();
    let mut best_yield = 65536usize;
    let mut best_rate = 0.0f64;
    for &y in &candidates {
        if let Some((rate, _, _)) = measure(y, true) {
            if rate > best_rate {
                best_rate = rate;
                best_yield = y;
            }
        }
        if t_start.elapsed().as_secs_f64() >= secs {
            break;
        }
    }

    if best_rate > 0.0 && t_start.elapsed().as_secs_f64() < secs {
        let mut confirm_runs = 0usize;
        let mut confirm_files = 0u64;
        let mut confirm_dt = 0.0f64;
        while t_start.elapsed().as_secs_f64() < secs {
            if let Some((_, files, dt)) = measure(best_yield, false) {
                confirm_runs += 1;
                confirm_files += files;
                confirm_dt += dt;
            } else {
                break;
            }
        }
        if confirm_runs > 0 {
            let avg_rate = (confirm_files as f64) / confirm_dt.max(1e-6);
            println!(
                "tune: confirm yield={best_yield} -> {avg_rate:.0} files/s avg over {confirm_runs} runs (files={confirm_files})"
            );
        }
    }
    println!("recommended.dir_yield_every={best_yield}");
    println!("hint.nvme=65536-131072, hint.hdd=8192-16384");

    fn shell_quote(token: &str) -> String {
        if token.is_empty() {
            return "\"\"".to_string();
        }
        if token.chars().any(|c| c.is_whitespace() || c == '"') {
            let escaped = token.replace('"', "\\\"");
            format!("\"{escaped}\"")
        } else {
            token.to_string()
        }
    }

    let wants_logical_only = !opt.compute_physical;
    let wants_approximate = opt.approximate_sizes;
    let wants_count_links = opt.count_hardlinks;
    let threads_recommended = opt.threads;

    let raw_args: Vec<String> = std::env::args().skip(1).collect();
    let mut filtered_args: Vec<String> = Vec::with_capacity(raw_args.len());
    let mut had_threads = false;
    let mut had_logical_only = false;
    let mut had_approximate = false;
    let mut had_count_links = false;
    #[cfg(target_os = "windows")]
    let mut had_win_ntquery = false;

    let mut i = 0;
    while i < raw_args.len() {
        let arg = &raw_args[i];
        if arg == "--tune-only" || arg.starts_with("--tune-only=") {
            i += 1;
            continue;
        }
        if arg == "--tune-secs" {
            i += 2;
            continue;
        }
        if arg.starts_with("--tune-secs=") {
            i += 1;
            continue;
        }
        if arg == "--dir-yield-every" {
            i += 2;
            continue;
        }
        if arg.starts_with("--dir-yield-every=") {
            i += 1;
            continue;
        }
        if arg == "--perf" {
            if let Some(next) = raw_args.get(i + 1) {
                if next.eq_ignore_ascii_case("turbo") {
                    i += 2;
                    continue;
                }
                filtered_args.push(arg.clone());
                filtered_args.push(next.clone());
                i += 2;
                continue;
            }
            i += 1;
            continue;
        }
        if let Some(value) = arg.strip_prefix("--perf=") {
            if value.eq_ignore_ascii_case("turbo") {
                i += 1;
                continue;
            }
            filtered_args.push(arg.clone());
            i += 1;
            continue;
        }
        if arg == "--threads" {
            had_threads = true;
            filtered_args.push(arg.clone());
            if let Some(next) = raw_args.get(i + 1) {
                filtered_args.push(next.clone());
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }
        if arg.starts_with("--threads=") {
            had_threads = true;
            filtered_args.push(arg.clone());
            i += 1;
            continue;
        }
        if arg == "--logical-only" {
            had_logical_only = true;
            filtered_args.push(arg.clone());
            i += 1;
            continue;
        }
        if arg == "--approximate" {
            had_approximate = true;
            filtered_args.push(arg.clone());
            i += 1;
            continue;
        }
        if arg == "--count-links" {
            had_count_links = true;
            filtered_args.push(arg.clone());
            i += 1;
            continue;
        }
        if arg == "--no-count-links" {
            had_count_links = true;
            filtered_args.push(arg.clone());
            i += 1;
            continue;
        }
        #[cfg(target_os = "windows")]
        if arg == "--win-ntquery" {
            had_win_ntquery = true;
            filtered_args.push(arg.clone());
            i += 1;
            continue;
        }
        filtered_args.push(arg.clone());
        i += 1;
    }

    let mut recommended_args: Vec<String> = Vec::new();
    if !had_threads && threads_recommended > 0 {
        recommended_args.push("--threads".to_string());
        recommended_args.push(threads_recommended.to_string());
    }
    if wants_logical_only && !had_logical_only {
        recommended_args.push("--logical-only".to_string());
    }
    if wants_approximate && !had_approximate {
        recommended_args.push("--approximate".to_string());
    }
    if wants_count_links && !had_count_links {
        recommended_args.push("--count-links".to_string());
    }
    #[cfg(target_os = "windows")]
    if !had_win_ntquery {
        recommended_args.push("--win-ntquery".to_string());
    }
    recommended_args.push("--dir-yield-every".to_string());
    recommended_args.push(best_yield.to_string());

    let exe_display = std::env::current_exe()
        .ok()
        .and_then(|p| p.file_name().map(|name| name.to_string_lossy().to_string()))
        .map(|name| format!("./{name}"))
        .unwrap_or_else(|| "./hyperdu".to_string());

    let mut pieces = Vec::with_capacity(1 + recommended_args.len() + filtered_args.len());
    pieces.push(shell_quote(&exe_display));
    for arg in &recommended_args {
        pieces.push(shell_quote(arg));
    }
    for arg in &filtered_args {
        pieces.push(shell_quote(arg));
    }
    let recommended_cmd = pieces.join(" ");
    println!("recommended.command={recommended_cmd}");
    Ok(())
}
