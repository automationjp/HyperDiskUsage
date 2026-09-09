# Benchmark build record

Source commit: `0a089c90c0e2995ef702ff053820d129265c3659`. Both release binaries were built from this commit; the Linux source was extracted from `git archive HEAD`. Core and CLI source plus Cargo.lock were clean at build time. Later changes affect GUI, packaging, documentation, site, benchmark argument validation and CLI help wording only; the measured core and CLI scan/output algorithms are unchanged.

| Platform | Compiler | Build | SHA-256 |
|---|---|---|---|
| Windows NTFS | rustc 1.98.1 | `cargo build --release --locked -p hyperdu -p hyperdu-gui` | `b4c4c4ca78119d61319c9b0a342ab7b7e7dfed3992a44fe56d88ddb5b6a9c3d8` |
| WSL2 Ubuntu 26.04 ext4 | rustc 1.97.1 | `cargo build --release --locked -p hyperdu` | `a43fff5f5b7a7663b15f081c1361256ef111461848f1fe49419b0e829bd32eb4` |

Each benchmark used a retained copy of its release binary, and verified the SHA-256 before and after all trials. The workspace release profile uses opt-level 3, thin LTO and one codegen unit; no target-cpu=native override was passed. Benchmark JSON records the actual executable, arguments, version, platform, raw timings, dataset totals and comparator version. Windows user-home paths are replaced by `%USERPROFILE%` in the public JSON.

Each dataset ran eight alternating warm trials. Windows registry: 111,813 files; Linux registry: 37,814 files. These are different workloads and cannot support an OS-to-OS speed claim.

This was a shared development PC, not an isolated benchmark host. Background tasks, including builds in other projects, may affect wall-clock measurements. No background process was terminated to run this benchmark. See the localized benchmark reports for limits and all raw samples.
