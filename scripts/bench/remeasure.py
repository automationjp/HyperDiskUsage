#!/usr/bin/env python3
"""Compare two HyperDU configurations on the same immutable corpus.

Creates only a new owned fixture directory, never clears caches or deletes input.
--entries 1000000 --shape flat --shape deep selects the large corpus.
Cold trials require an explicit --cache-reset JSON argv, executed before each run.
The independent oracle and reporting are outside timing; each timed command has
the same scan + rollup + full JSON report workload. Hosted/WSL results are labeled.
"""
from __future__ import annotations

import argparse
import ctypes
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import signal
import stat
import statistics
import subprocess
import sys
import tempfile
import time

from du_same_conditions import file_hash


def clean_environment(overrides: dict[str, str] | None = None) -> dict[str, str]:
    keep = ("PATH", "SystemRoot", "SystemDrive", "WINDIR", "HOME", "USERPROFILE",
            "TMP", "TEMP", "TMPDIR")
    env = {key: os.environ[key] for key in keep if key in os.environ}
    env.update(LC_ALL="C", LANG="C", TZ="UTC")
    for key, value in (overrides or {}).items():
        if not key.startswith("HYPERDU_") or not isinstance(value, str):
            raise ValueError("scan overrides must be HYPERDU_* string values")
        env[key] = value
    return env


def windows_metrics(handle: int) -> dict:
    from ctypes import wintypes as w

    class Memory(ctypes.Structure):
        _fields_ = [("cb", w.DWORD), ("faults", w.DWORD)] + [
            (name, ctypes.c_size_t) for name in (
                "peak_rss", "rss", "peak_pool_paged", "pool_paged",
                "peak_pool_nonpaged", "pool_nonpaged", "pagefile", "peak_pagefile")]

    class Io(ctypes.Structure):
        _fields_ = [(name, ctypes.c_ulonglong) for name in (
            "reads", "writes", "other", "read_bytes", "write_bytes", "other_bytes")]

    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    memory_api = ctypes.WinDLL("psapi", use_last_error=True).GetProcessMemoryInfo
    memory_api.argtypes = [w.HANDLE, ctypes.POINTER(Memory), w.DWORD]
    memory_api.restype = w.BOOL
    kernel.GetProcessTimes.argtypes = [w.HANDLE] + [ctypes.POINTER(w.FILETIME)] * 4
    kernel.GetProcessTimes.restype = w.BOOL
    kernel.GetProcessIoCounters.argtypes = [w.HANDLE, ctypes.POINTER(Io)]
    kernel.GetProcessIoCounters.restype = w.BOOL
    created, exited, system, user = (w.FILETIME() for _ in range(4))
    memory, io = Memory(), Io()
    memory.cb = ctypes.sizeof(memory)
    for succeeded in (
        kernel.GetProcessTimes(handle, ctypes.byref(created), ctypes.byref(exited),
                               ctypes.byref(system), ctypes.byref(user)),
        memory_api(handle, ctypes.byref(memory), memory.cb),
        kernel.GetProcessIoCounters(handle, ctypes.byref(io)),
    ):
        if not succeeded:
            raise ctypes.WinError(ctypes.get_last_error())
    seconds = lambda value: ((value.dwHighDateTime << 32) | value.dwLowDateTime) / 10_000_000
    return dict(user_seconds=seconds(user), kernel_seconds=seconds(system),
                peak_rss_bytes=memory.peak_rss, page_faults=memory.faults,
                minor_faults=None, major_faults=None, bytes_read=io.read_bytes,
                bytes_read_method="GetProcessIoCounters: process read transfers, not device bytes")


def measure_process(command: list[str], env: dict[str, str], timeout: float) -> tuple[dict, bytes, bytes]:
    """Wait on this child only; no accumulated RUSAGE_CHILDREN or sampled RSS."""
    start = time.perf_counter()
    with tempfile.TemporaryFile() as out, tempfile.TemporaryFile() as err:
        process = subprocess.Popen(command, stdout=out, stderr=err, env=env,
                                   start_new_session=os.name != "nt")
        try:
            if os.name == "nt":
                process.wait(timeout=timeout)
                metrics = windows_metrics(int(process._handle))
            else:
                io_bytes = None
                usage = None
                while True:
                    if sys.platform == "linux":
                        # Leave the exited child waitable while reading its final
                        # I/O counters. Sampling a live PID can miss the entire scan.
                        ended = os.waitid(os.P_PID, process.pid,
                                          os.WEXITED | os.WNOHANG | os.WNOWAIT)
                        if ended is not None:
                            try:
                                io_text = Path(f"/proc/{process.pid}/io").read_text()
                                io_bytes = int(re.search(r"^read_bytes:\s*(\d+)$", io_text, re.M)[1])
                            except (OSError, TypeError, ValueError):
                                pass
                            _, status, usage = os.wait4(process.pid, 0)
                            process.returncode = os.waitstatus_to_exitcode(status)
                            break
                    else:
                        pid, status, usage = os.wait4(process.pid, os.WNOHANG)
                        if pid:
                            process.returncode = os.waitstatus_to_exitcode(status)
                            break
                    if time.perf_counter() - start >= timeout:
                        raise subprocess.TimeoutExpired(command, timeout)
                    time.sleep(0.002)
                metrics = dict(
                    user_seconds=usage.ru_utime, kernel_seconds=usage.ru_stime,
                    peak_rss_bytes=usage.ru_maxrss * (1 if sys.platform == "darwin" else 1024),
                    page_faults=usage.ru_minflt + usage.ru_majflt,
                    minor_faults=usage.ru_minflt, major_faults=usage.ru_majflt,
                    bytes_read=io_bytes, bytes_read_method=(
                        "/proc/pid/io read_bytes: storage-layer reads" if io_bytes is not None
                        else "unavailable: this OS did not expose final process storage bytes"),
                    input_blocks=usage.ru_inblock, output_blocks=usage.ru_oublock)
            metrics["wall_seconds"] = time.perf_counter() - start
            metrics["cpu_seconds"] = metrics["user_seconds"] + metrics["kernel_seconds"]
            metrics["command"] = command
            metrics["exit_code"] = process.returncode
            out.seek(0)
            err.seek(0)
            stdout, stderr = out.read(), err.read()
            if process.returncode != 0:
                raise RuntimeError(f"exit={process.returncode}: {command!r}\n{stderr.decode(errors='replace')}")
            return metrics, stdout, stderr
        finally:
            if process.returncode is None:
                if os.name == "nt":
                    process.kill()
                else:
                    try:
                        os.killpg(process.pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
                process.wait()
            # Popen retains the Windows process handle until object destruction.


def windows_allocated(path: Path) -> int:
    from ctypes import wintypes as w

    class Standard(ctypes.Structure):
        _fields_ = [("allocation", ctypes.c_longlong), ("size", ctypes.c_longlong),
                    ("links", w.DWORD), ("delete_pending", ctypes.c_ubyte),
                    ("directory", ctypes.c_ubyte)]

    api = ctypes.WinDLL("kernel32", use_last_error=True)
    api.CreateFileW.argtypes = [w.LPCWSTR, w.DWORD, w.DWORD, ctypes.c_void_p,
                               w.DWORD, w.DWORD, w.HANDLE]
    api.CreateFileW.restype = w.HANDLE
    api.GetFileInformationByHandleEx.argtypes = [w.HANDLE, ctypes.c_int, ctypes.c_void_p, w.DWORD]
    api.GetFileInformationByHandleEx.restype = w.BOOL
    api.CloseHandle.argtypes = [w.HANDLE]
    api.CloseHandle.restype = w.BOOL
    handle = api.CreateFileW(str(path), 0x80, 7, None, 3, 0x00200000, None)
    if handle == ctypes.c_void_p(-1).value:
        raise ctypes.WinError(ctypes.get_last_error())
    try:
        info = Standard()
        if not api.GetFileInformationByHandleEx(handle, 1, ctypes.byref(info), ctypes.sizeof(info)):
            raise ctypes.WinError(ctypes.get_last_error())
        if info.allocation < 0:
            raise ValueError(f"negative allocation: {path}")
        return info.allocation
    finally:
        api.CloseHandle(handle)


def oracle(root: Path) -> dict:
    """Independent no-follow, one-filesystem regular-file accounting and digest."""
    device = root.stat().st_dev
    seen: set[tuple[int, int]] = set()
    rows: dict[str, list[int]] = {}
    digest = hashlib.sha256()
    entries_seen = 0
    stack = [root]
    while stack:
        directory = stack.pop()
        key = directory.relative_to(root).as_posix()
        rows.setdefault(key, [0, 0, 0])
        with os.scandir(directory) as stream:
            entries = sorted(stream, key=lambda entry: entry.name)
        for entry in entries:
            # DirEntry.stat leaves identity fields zero on some Windows Python builds.
            info = os.stat(entry.path, follow_symlinks=False)
            path = Path(entry.path)
            relative = path.relative_to(root).as_posix()
            entries_seen += 1
            digest.update(json.dumps([
                relative, info.st_dev, info.st_ino, info.st_mode, info.st_nlink,
                info.st_size, getattr(info, "st_blocks", None),
                info.st_mtime_ns, info.st_ctime_ns,
                getattr(info, "st_file_attributes", None),
            ], ensure_ascii=True).encode() + b"\n")
            reparse = bool(getattr(info, "st_file_attributes", 0) & 0x400)
            if info.st_dev != device or reparse:
                continue
            if stat.S_ISDIR(info.st_mode):
                stack.append(path)
            elif stat.S_ISREG(info.st_mode):
                identity = info.st_dev, info.st_ino
                if identity in seen:
                    continue
                seen.add(identity)
                allocated = windows_allocated(path) if os.name == "nt" else info.st_blocks * 512
                row = rows[key]
                row[0] += info.st_size
                row[1] += allocated
                row[2] += 1
    for key in sorted(rows, key=lambda value: len(Path(value).parts), reverse=True):
        if key != ".":
            parent = Path(key).parent.as_posix()
            rows[parent] = [a + b for a, b in zip(rows[parent], rows[key])]
    return dict(rows=rows, digest=digest.hexdigest(), entries=entries_seen,
                directories=len(rows), root=rows["."])


def make_tree(parent: Path, shape: str, entries: int, depth: int, file_size: int) -> Path:
    root = Path(tempfile.mkdtemp(prefix=f"hyperdu-{shape}-", dir=parent)).resolve()
    if os.name == "nt" and not str(root).startswith("\\\\?\\"):
        root = Path("\\\\?\\" + str(root))
    payload = bytes(range(256)) * (file_size // 256) + bytes(range(file_size % 256))
    previous_group = None
    directory = root
    for index in range(entries):
        group = index // 128
        if group != previous_group:
            if shape == "wide":
                directory = root / f"d{group}"
            elif shape == "deep":
                parts = [f"d{(group >> (2 * digit)) & 3}" for digit in reversed(range(depth))]
                directory = root.joinpath(*parts)
            directory.mkdir(parents=True, exist_ok=True)
            previous_group = group
        (directory / f"f{index}.bin").write_bytes(payload)
    return root


def parse_report(path: Path, root: Path) -> dict[str, list[int]]:
    rows: dict[str, list[int]] = {}
    for row in json.loads(path.read_text(encoding="utf-8")):
        name = Path(row["path"])
        if not name.is_absolute() or ".." in name.parts:
            raise ValueError(f"invalid report path: {name}")
        key = name.relative_to(root).as_posix()
        if key in rows:
            raise ValueError(f"duplicate report row: {key}")
        values = [row[field] for field in ("logical", "physical", "files")]
        if any(type(value) is not int or value < 0 for value in values):
            raise ValueError(f"invalid integer accounting: {row}")
        rows[key] = values
    if "." not in rows:
        raise ValueError("report omitted the root")
    return rows


def verify_rows(actual: dict, expected: dict) -> None:
    if actual != expected:
        keys = sorted(actual.keys() | expected.keys())
        differences = [(key, actual.get(key), expected.get(key)) for key in keys
                       if actual.get(key) != expected.get(key)]
        raise ValueError(f"logical/allocated/file-count parity failed: {differences[:5]}")


def scan_command(binary: Path, root: Path, output: Path, directories: int, flags: list[str]) -> list[str]:
    return [str(binary), str(root), "--exclude", "", "--one-file-system",
            "--progress-every", "0", "--top", str(directories), "--json", str(output), *flags]


def checked_scan(command: list[str], env: dict[str, str], timeout: float) -> dict:
    metrics, _, stderr = measure_process(command, env, timeout)
    diagnostics = stderr.decode("utf-8", errors="replace")
    if any(not line.startswith("fs-auto: ") for line in diagnostics.splitlines()):
        raise ValueError(f"scanner reported a warning/error: {diagnostics}")
    metrics["diagnostics"] = diagnostics
    return metrics


def syscall_count(raw: str) -> int:
    match = re.search(r"^\s*100\.00\s+\S+\s+\S+\s+(\d+)(?:\s+\d+)?\s+total\s*$", raw, re.M)
    if match is None:
        raise ValueError("strace summary has no total count")
    return int(match[1])


def syscall_probe(command: list[str], env: dict[str, str], timeout: float, scratch: Path) -> dict:
    executable = shutil.which("strace") if sys.platform == "linux" else None
    if executable is None:
        return dict(count=None, reason="unavailable: strace supported only on Linux",
                    timed=False)
    trace = scratch / "syscalls.txt"
    checked_scan([executable, "-f", "-c", "-o", str(trace), "--", *command], env, timeout)
    raw = trace.read_text()
    return dict(count=syscall_count(raw), raw=raw, timed=False)


def save(path: Path, result: dict) -> None:
    partial = path.with_suffix(path.suffix + ".partial")
    partial.write_text(json.dumps(result, indent=2, ensure_ascii=True) + "\n", encoding="utf-8")
    partial.replace(path)


def source_record(source: Path, commit: str) -> dict:
    def git(*args: str) -> bytes:
        return subprocess.check_output(["git", "-C", str(source), *args], stderr=subprocess.PIPE)
    head = git("rev-parse", "HEAD").decode().strip()
    if head != commit:
        raise ValueError(f"source HEAD {head} does not match --commit {commit}")
    diff = git("diff", "HEAD", "--binary")
    status = git("status", "--porcelain", "--untracked-files=normal").decode()
    return dict(head=head, tree=git("rev-parse", "HEAD^{tree}").decode().strip(),
                tracked_diff_sha256=hashlib.sha256(diff).hexdigest(),
                status=status, clean=not status, cargo_lock_sha256=file_hash(source / "Cargo.lock"))


def parse_json_map(raw: str) -> dict[str, str]:
    value = json.loads(raw)
    if not isinstance(value, dict):
        raise ValueError("environment overrides must be a JSON object")
    clean_environment(value)
    return value


def parse_flags(raw: str) -> list[str]:
    flags = json.loads(raw)
    if not isinstance(flags, list) or not all(isinstance(flag, str) for flag in flags):
        raise ValueError("flags must be a JSON argv array")
    index = 0
    while index < len(flags):
        if flags[index] == "--mft":
            index += 1
        elif flags[index] in ("--threads", "--io-profile", "--prefetch"):
            index += 2
            if index > len(flags):
                raise ValueError("missing performance option value")
        else:
            raise ValueError(f"option can change the accounting workload: {flags[index]}")
    return flags


def baseline_record(path: Path | None, binary: Path, commit: str) -> dict:
    record = json.loads(path.read_text(encoding="utf-8")) if path else {}
    actual_hash = file_hash(binary)
    if path and (record.get("binary_sha256") != actual_hash or record.get("head") != commit
                 or not record.get("build_command")):
        raise ValueError("build record must bind head, binary_sha256 and build_command")
    return dict(head=commit, binary_sha256=actual_hash,
                build_record=record or None, build_provenance="record" if record else "caller assertion")


def validate_locations(source: Path, output: Path, parent: Path, roots: list[Path]) -> None:
    """Reject all write/input overlaps before creating any directory."""
    for destination in (output, parent):
        if destination.is_relative_to(source) or any(destination.is_relative_to(root) for root in roots):
            raise ValueError(f"keep benchmark writes outside source and measured inputs: {destination}")

def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bin", type=Path, required=True)
    parser.add_argument("--baseline-bin", type=Path)
    parser.add_argument("--commit", required=True)
    parser.add_argument("--baseline-commit")
    parser.add_argument("--build-record", type=Path, required=True)
    parser.add_argument("--baseline-build-record", type=Path)
    parser.add_argument("--source-root", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--dataset-parent", type=Path, required=True)
    parser.add_argument("--tree", type=Path, action="append", default=[])
    parser.add_argument("--shape", choices=("flat", "wide", "deep"), action="append", default=[])
    parser.add_argument("--entries", type=int, default=10000)
    parser.add_argument("--depth", type=int, default=16)
    parser.add_argument("--file-size", type=int, default=256)
    parser.add_argument("--runs", type=int, default=8)
    parser.add_argument("--timeout", type=float, default=300)
    parser.add_argument("--cache", choices=("warm", "cold"), default="warm")
    parser.add_argument("--cache-reset", help="JSON argv explicitly supplied for each cold trial")
    parser.add_argument("--storage-kind", choices=("unknown", "nvme", "ssd", "hdd", "network"), default="unknown")
    parser.add_argument("--environment-label", required=True)
    parser.add_argument("--candidate-env", default="{}")
    parser.add_argument("--baseline-env", default="{}")
    parser.add_argument("--candidate-flags", default="[]")
    parser.add_argument("--baseline-flags", default="[]")
    parser.add_argument("--syscalls", choices=("auto", "off"), default="auto")
    args = parser.parse_args()
    if args.runs < 4 or args.runs % 2 or args.timeout <= 0:
        parser.error("require an even number of at least four trials and a positive timeout")
    if args.entries < 1 or not 1 <= args.depth <= 64 or not 0 <= args.file_size <= 1048576:
        parser.error("invalid corpus entry count, depth or file size")
    reset = json.loads(args.cache_reset) if args.cache_reset else None
    if args.cache == "cold" and not reset:
        parser.error("cold trials require an explicit --cache-reset JSON argv")
    if reset and (not isinstance(reset, list) or not all(isinstance(v, str) for v in reset)):
        parser.error("--cache-reset must be a JSON argv")
    if args.cache == "warm" and reset:
        parser.error("--cache-reset is only valid for cold trials")
    baseline = (args.baseline_bin or args.bin).resolve(strict=True)
    candidate = args.bin.resolve(strict=True)
    if baseline != candidate and not args.baseline_commit:
        parser.error("a separate baseline executable requires --baseline-commit")
    binaries = dict(candidate=candidate, baseline=baseline)
    overrides = dict(candidate=parse_json_map(args.candidate_env),
                     baseline=parse_json_map(args.baseline_env))
    flags = dict(candidate=parse_flags(args.candidate_flags),
                 baseline=parse_flags(args.baseline_flags))
    if baseline == candidate and overrides["candidate"] == overrides["baseline"] and flags["candidate"] == flags["baseline"]:
        parser.error("supply a distinct baseline binary or configuration")
    output = args.output.resolve()
    if output.exists() or output.with_suffix(output.suffix + ".partial").exists():
        parser.error("results are immutable; choose a new --output")
    for binary in binaries.values():
        if not binary.is_file() or (binary.parent / "hyperdu-config.json").exists():
            parser.error("use isolated executables without hyperdu-config.json")
    source_root = args.source_root.resolve(strict=True)
    parent = args.dataset_parent.resolve()
    trees = [(path.name, path.resolve(strict=True)) for path in args.tree]
    validate_locations(source_root, output, parent, [root for _, root in trees])
    source = source_record(source_root, args.commit)
    if not source["clean"]:
        raise ValueError("performance measurements require a clean committed source snapshot")
    records = dict(
        candidate=baseline_record(args.build_record, candidate, args.commit),
        baseline=baseline_record(args.baseline_build_record or (args.build_record if baseline == candidate else None), baseline,
                                 args.baseline_commit or args.commit))
    if baseline != candidate and args.baseline_build_record is None:
        parser.error("a separate baseline requires --baseline-build-record")
    parent.mkdir(parents=True, exist_ok=True)
    output.parent.mkdir(parents=True, exist_ok=True)
    shapes = args.shape or ([] if trees else ["flat", "wide", "deep"])
    for shape in dict.fromkeys(shapes):
        trees.append((shape, make_tree(parent, shape, args.entries, args.depth, args.file_size)))
    for _, root in trees:
        if not root.is_dir() or output.is_relative_to(root) or any(
                binary.is_relative_to(root) for binary in binaries.values()):
            parser.error("input must be a directory; keep executable and results outside it")
    result = dict(
        schema_version=2, complete=False, started_at=time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        platform=platform.platform(), machine=platform.machine(),
        environment_label=args.environment_label, storage_kind=args.storage_kind,
        storage_kind_provenance="operator label", cache=args.cache,
        cache_evidence="per-trial explicit reset command" if reset else "untimed warm-up before trials",
        cache_reset=reset, source=source, binaries=records, runs=args.runs,
        overrides=overrides, flags=flags, datasets=[],
        timing_scope="scan + rollup + full directory JSON/console report",
        accounting="regular-file logical/allocated bytes, hardlinks once, no-follow, one filesystem",
        generated=dict(entries=args.entries, depth=args.depth, file_size=args.file_size, shapes=shapes))
    save(output, result)
    try:
        with tempfile.TemporaryDirectory(prefix="hyperdu-gate-") as temporary:
            scratch = Path(temporary)
            for name, root in trees:
                if scratch.is_relative_to(root):
                    raise ValueError("temporary reports must be outside the measured input")
                expected = oracle(root)
                dataset = dict(name=name, root=str(root), entries=expected["entries"],
                               directories=expected["directories"], expected_root=expected["root"],
                               accounted_files=expected["root"][2],
                               entries_definition="names enumerated in traversed directories, including skipped names; no skipped descendants",
                               fingerprint=expected["digest"], samples=[], syscalls={})
                result["datasets"].append(dataset)
                report_paths = {tool: scratch / f"{tool}.json" for tool in binaries}
                commands = {tool: scan_command(binary, root, report_paths[tool],
                                                expected["directories"], flags[tool])
                            for tool, binary in binaries.items()}
                envs = {tool: clean_environment(overrides[tool]) for tool in binaries}
                # Warm up and reject the wrong/partial baseline before any timing.
                for tool in binaries:
                    checked_scan(commands[tool], envs[tool], args.timeout)
                    verify_rows(parse_report(report_paths[tool], root), expected["rows"])
                for iteration in range(args.runs):
                    for tool in (("baseline", "candidate") if iteration % 2 else ("candidate", "baseline")):
                        if reset:
                            _, reset_out, reset_err = measure_process(reset, clean_environment(), args.timeout)
                            dataset.setdefault("cache_resets", []).append(dict(
                                iteration=iteration, tool=tool,
                                stdout=reset_out.decode(errors="replace"), stderr=reset_err.decode(errors="replace")))
                        metrics = checked_scan(commands[tool], envs[tool], args.timeout)
                        verify_rows(parse_report(report_paths[tool], root), expected["rows"])
                        metrics.update(tool=tool, iteration=iteration,
                                       entries_per_second=expected["entries"] / metrics["wall_seconds"])
                        dataset["samples"].append(metrics)
                        save(output, result)
                if args.syscalls != "off":
                    for tool in binaries:
                        probe = syscall_probe(commands[tool], envs[tool], args.timeout, scratch)
                        if probe["count"] is not None:
                            verify_rows(parse_report(report_paths[tool], root), expected["rows"])
                        probe["per_entry"] = (probe["count"] / expected["entries"]
                                              if probe["count"] is not None and expected["entries"] else None)
                        dataset["syscalls"][tool] = probe
                if oracle(root) != expected:
                    raise ValueError(f"dataset changed during measurement: {root}")
                dataset["parity"] = "PASS: every directory logical/allocated/files matches independent metadata"
                dataset["median_seconds"] = {
                    tool: statistics.median(s["wall_seconds"] for s in dataset["samples"] if s["tool"] == tool)
                    for tool in binaries}
                dataset["baseline_over_candidate"] = (
                    dataset["median_seconds"]["baseline"] / dataset["median_seconds"]["candidate"])
                save(output, result)
                print(name, dataset["median_seconds"], flush=True)
        for tool, binary in binaries.items():
            if file_hash(binary) != records[tool]["binary_sha256"]:
                raise ValueError(f"{tool} binary changed during measurement")
        if source_record(args.source_root.resolve(), args.commit) != source:
            raise ValueError("source snapshot changed during measurement")
        result["complete"] = True
        return 0
    except Exception as error:
        result["error"] = str(error)
        raise
    finally:
        result["finished_at"] = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
        save(output, result)


if __name__ == "__main__":
    raise SystemExit(main())
