#!/usr/bin/env python3
"""Warm, alternating end-to-end timings with raw samples and workload gates.

No cache dropping or deletion. Creates a fresh synthetic dataset directory.
The caller must build the binary from --commit and retain its build record;
this harness verifies binary stability, not the asserted source provenance.
Run with --help; pass the exact release binary built from --commit.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import statistics
import shutil
import subprocess
import tempfile
import time


def run(command, robocopy=False):
    start = time.perf_counter_ns()
    result = subprocess.run(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    elapsed = (time.perf_counter_ns() - start) / 1_000_000
    # Robocopy's low bits describe differences, not process failure.
    if (not 0 <= result.returncode < 8) if robocopy else (result.returncode != 0):
        raise RuntimeError(f"{command!r}: exit {result.returncode}: {result.stderr!r} stdout={result.stdout!r}")
    return elapsed, result.stdout.decode("utf-8", errors="replace")


def oracle(root):
    """Independent lstat walk, same-device regular files, folding hardlinks."""
    device = root.stat().st_dev
    seen = set()
    out = dict(files=0, logical=0, physical=0, entries=0, directories=0, directory_and_link_physical=0)
    stack = [root]
    while stack:
        directory = stack.pop()
        out["directories"] += 1
        out["directory_and_link_physical"] += getattr(directory.stat(), "st_blocks", 0) * 512
        with os.scandir(directory) as entries:
            for entry in entries:
                stat = os.stat(entry.path, follow_symlinks=False)
                if stat.st_dev != device:
                    continue
                if entry.is_dir(follow_symlinks=False):
                    stack.append(Path(entry.path))
                elif entry.is_file(follow_symlinks=False):
                    out["entries"] += 1
                    identity = (stat.st_dev, stat.st_ino)
                    if identity not in seen:
                        seen.add(identity)
                        out["files"] += 1
                        out["logical"] += stat.st_size
                        out["physical"] += getattr(stat, "st_blocks", 0) * 512
                elif entry.is_symlink():
                    out["directory_and_link_physical"] += getattr(stat, "st_blocks", 0) * 512
    if os.name == "nt":
        out["physical"] = None
        out["directory_and_link_physical"] = None
    return out


def make_trees(parent):
    root = Path(tempfile.mkdtemp(prefix="hyperdu-warm-", dir=parent))
    if os.name == "nt":
        root = Path("\\\\?\\" + str(root.resolve()))
    for kind in ("wide", "deep", "flat"):
        (root / kind).mkdir()
    payload = bytes(range(256)) * 16
    for i in range(500):
        directory = root / "wide" / str(i)
        directory.mkdir()
        for j in range(40):
            (directory / str(j)).write_bytes(payload)
    directory = root / "deep"
    # One-character components keep the Windows path below the NT limit.
    for _ in range(400):
        directory /= "d"
        directory.mkdir()
        for j in range(5):
            (directory / str(j)).write_bytes(payload)
    for i in range(20_000):
        (root / "flat" / str(i)).write_bytes(payload)
    return [(name, root / name) for name in ("wide", "deep", "flat")]


def measure(binary, name, root, runs, scratch):
    hd = [str(binary), str(root), "--top", "1", "--exclude", "", "--one-file-system"]
    windows = os.name == "nt"
    baseline_root = str(root).removeprefix("\\\\?\\")
    baseline_executable = shutil.which("robocopy" if windows else "du")
    if baseline_executable is None:
        raise RuntimeError("comparison tool not found")
    baseline = ([baseline_executable, baseline_root, str(scratch / "unused-destination"),
                 "/L", "/S", "/XJ", "/BYTES", "/NFL", "/NDL", "/NJH", "/NP", "/R:0", "/W:0"]
                if windows else [baseline_executable, "-sx", "--block-size=1", str(root)])
    expected = oracle(root)
    gate_path = scratch / "gate.json"
    _, hd_output = run(hd + ["--json", str(gate_path)])
    rows = json.loads(gate_path.read_text(encoding="utf-8"))
    actual = next(row for row in rows if Path(row["path"]) == root)
    directory_count = re.search(r"Total:.*dirs=(\d+)", hd_output)
    if not directory_count or int(directory_count[1]) != expected["directories"]:
        raise RuntimeError(f"{name}: directory count differs from oracle")
    for key in ("files", "logical"):
        if expected[key] != actual[key]:
            raise RuntimeError(f"{name}: {key}: oracle={expected[key]} hyperdu={actual[key]}")
    if not windows and expected["physical"] != actual["physical"]:
        raise RuntimeError(f"{name}: physical bytes differ from lstat oracle")
    _, baseline_output = run(baseline, windows)
    if windows:
        # The first three numeric summary rows are directories, files, bytes.
        numeric = re.findall(r"^\s*[^:\r\n]+:\s+(\d+)\s+(\d+)\s+(\d+)\s+(\d+)\s+(\d+)\s+(\d+)\s*$",
                             baseline_output, re.MULTILINE)
        if len(numeric) != 3 or int(numeric[1][0]) != actual["files"] or int(numeric[2][0]) != actual["logical"]:
            raise RuntimeError(f"{name}: robocopy workload mismatch: {baseline_output}")
    else:
        baseline_bytes = int(baseline_output.split()[0])
        if baseline_bytes != expected["physical"] + expected["directory_and_link_physical"]:
            raise RuntimeError(f"{name}: du allocated bytes do not match the oracle")
    samples = {"hyperdu": [], "baseline": []}
    for iteration in range(runs):
        order = ("hyperdu", "baseline") if iteration % 2 == 0 else ("baseline", "hyperdu")
        for tool in order:
            command = hd if tool == "hyperdu" else baseline
            elapsed, _ = run(command, windows and tool == "baseline")
            samples[tool].append(round(elapsed, 3))
    # Reject a dataset modified during timing instead of publishing a fast partial scan.
    if oracle(root) != expected:
        raise RuntimeError(f"{name}: dataset changed during timing")
    run(hd + ["--json", str(gate_path)])
    if json.loads(gate_path.read_text(encoding="utf-8")) != rows:
        raise RuntimeError(f"{name}: HyperDU totals changed during timing")
    medians = {tool: round(statistics.median(values), 3) for tool, values in samples.items()}
    return dict(name=name, root=str(root), commands=dict(hyperdu=hd, baseline=baseline),
                oracle=expected, physical_unverified=windows, hyperdu=actual, baseline_gate_output=baseline_output,
                hyperdu_gate_output=hd_output, samples_ms=samples, median_ms=medians,
                ratio=round(medians["baseline"] / medians["hyperdu"], 3))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bin", type=Path, required=True)
    parser.add_argument("--commit", required=True, help="Source commit from the build record; caller must verify provenance")
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--dataset-parent", type=Path, required=True)
    parser.add_argument("--runs", type=int, default=8)
    parser.add_argument("--tree", type=Path, action="append", default=[])
    parser.add_argument("--synthetic-root", type=Path, help="Reuse the wide/deep/flat fixtures under this directory")
    args = parser.parse_args()
    if args.runs < 4 or args.runs % 2:
        parser.error("an even number of at least four repetitions is required for balanced ordering")
    if not args.bin.is_file():
        parser.error("release binary does not exist")
    args.dataset_parent.mkdir(parents=True, exist_ok=True)
    trees = ([(name, args.synthetic_root / name) for name in ("wide", "deep", "flat")]
             if args.synthetic_root else make_trees(args.dataset_parent))
    trees += [(path.name, path.resolve()) for path in args.tree]
    baseline_version = (run(["powershell", "-NoProfile", "-Command",
                             "(Get-Item (Get-Command robocopy.exe).Source).VersionInfo.FileVersion"])[1].strip() if os.name == "nt"
                        else run(["du", "--version"])[1].splitlines()[0])
    result = dict(baseline_environment=baseline_version, commit=args.commit, date=time.strftime("%Y-%m-%d"), cache="warm",
                  platform=platform.platform(), runs=args.runs, complete=False,
                  binary_sha256=hashlib.sha256(args.bin.read_bytes()).hexdigest(),
                  binary_version=run([str(args.bin), "--version"])[1].strip(), datasets=[])
    args.output.parent.mkdir(parents=True, exist_ok=True)
    scratch = Path(tempfile.mkdtemp(prefix="hyperdu-gate-", dir=args.dataset_parent))
    for name, root in trees:
        measured = measure(args.bin.resolve(), name, root, args.runs, scratch)
        result["datasets"].append(measured)
        args.output.write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
        print(name, measured["median_ms"], measured["ratio"], flush=True)
    if hashlib.sha256(args.bin.read_bytes()).hexdigest() != result["binary_sha256"]:
        raise RuntimeError("binary changed during benchmark")
    result["complete"] = True
    args.output.write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
