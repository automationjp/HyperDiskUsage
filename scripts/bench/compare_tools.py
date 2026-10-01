#!/usr/bin/env python3
"""Warm, alternating end-to-end timings of HyperDU, dua-cli and tokei on the same trees.

No cache dropping and no dataset mutation. Every timing is process start to exit with
stdout piped, so each tool's own output cost is included. The independent lstat oracle
(remeasure.oracle) gates the byte totals of HyperDU and dua before timing and must be
unchanged afterwards. tokei counts source lines, not bytes: it has no byte total to
gate, so its samples are a different-workload reference and never a like-for-like ratio.
"""

import argparse
import hashlib
import json
import os
import platform
import re
import shutil
import statistics
import tempfile
import time
from pathlib import Path

from remeasure import make_trees, oracle, run

TOOLS = ("hyperdu", "dua", "tokei")


def commands(binaries, root):
    """Return the timed argv of each tool for one tree; HyperDU keeps the extended-length path."""
    # dua and tokei reject the extended-length prefix that lets the deep fixture pass MAX_PATH.
    plain = str(root).removeprefix("\\\\?\\")
    return {
        "hyperdu": [str(binaries["hyperdu"]), str(root), "--top", "1", "--exclude", "", "--one-file-system"],
        # Flat listing: `-d` tree mode adds directory entry sizes and disagrees with the oracle.
        "dua": [str(binaries["dua"]), "aggregate", "-A", "-x", "-f", "bytes", plain],
        "tokei": [str(binaries["tokei"]), plain, "--output", "json"],
    }


def gate(name, root, expected, cmds, scratch):
    """Return per-tool gate facts; raise if a byte total disagrees with the oracle."""
    facts = {}
    gate_path = scratch / "gate.json"
    run(cmds["hyperdu"] + ["--json", str(gate_path)])
    actual = next(row for row in json.loads(gate_path.read_text(encoding="utf-8"))
                  if Path(row["path"]) == root)
    for key in ("files", "logical"):
        if actual[key] != expected[key]:
            raise RuntimeError(f"{name}: {key}: oracle={expected[key]} hyperdu={actual[key]}")
    facts["hyperdu"] = dict(files=actual["files"], logical=actual["logical"])
    _, output = run(cmds["dua"])
    rows = re.findall(r"^\s*(\d+) b (.+)$", output, re.MULTILINE)
    # dua prints a `total` row only when the input has several entries.
    total = int(rows[-1][0]) if rows and (rows[-1][1] == "total" or len(rows) == 1) else None
    if total != expected["logical"]:
        raise RuntimeError(f"{name}: dua total {total} != oracle {expected['logical']}")
    facts["dua"] = dict(logical=total, output_lines=len(output.splitlines()))
    _, output = run(cmds["tokei"])
    report = json.loads(output)
    facts["tokei"] = dict(files=sum(len(v["reports"]) for k, v in report.items() if k != "Total"),
                          code_lines=sum(v["code"] for k, v in report.items() if k != "Total"))
    return facts


def measure(binaries, name, root, runs, scratch, tools):
    """Gate one tree against the oracle, time `tools` in rotating order, and return the raw record."""
    cmds = commands(binaries, root)
    expected = oracle(root)
    facts = gate(name, root, expected, cmds, scratch)
    samples = {tool: [] for tool in tools}
    for iteration in range(runs):
        shift = iteration % len(tools)
        for tool in tools[shift:] + tools[:shift]:
            elapsed, _ = run(cmds[tool])
            samples[tool].append(round(elapsed, 3))
    if oracle(root) != expected:
        raise RuntimeError(f"{name}: dataset changed during timing")
    medians = {tool: round(statistics.median(v), 3) for tool, v in samples.items()}
    minimums = {tool: round(min(v), 3) for tool, v in samples.items()}
    return dict(name=name, root=str(root), commands=cmds, oracle=expected, gate=facts,
                samples_ms=samples, median_ms=medians, min_ms=minimums,
                ratio_dua=round(medians["dua"] / medians["hyperdu"], 3) if "dua" in tools else None,
                ratio_tokei_reference=round(medians["tokei"] / medians["hyperdu"], 3) if "tokei" in tools else None)


def version(binary, flag="--version"):
    """Return the version banner a tool binary prints."""
    return run([str(binary), flag])[1].strip()


def main():
    """Parse arguments, measure every synthetic and real tree, and write the JSON record."""
    parser = argparse.ArgumentParser(description=__doc__)
    for tool in TOOLS:
        parser.add_argument(f"--{tool}", type=Path, required=True)
    parser.add_argument("--commit", required=True, help="Source commit HyperDU was built from")
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--dataset-parent", type=Path, required=True)
    parser.add_argument("--runs", type=int, default=12)
    parser.add_argument("--files", type=int, default=100_000, help="Files per synthetic shape")
    parser.add_argument("--tree", action="append", default=[], metavar="NAME=PATH",
                        help="Real tree; tokei is timed on these only")
    parser.add_argument("--note", default="", help="Free-text conditions worth keeping with the record")
    parser.add_argument("--synthetic-root", type=Path, help="Reuse wide/deep/flat fixtures under this directory")
    args = parser.parse_args()
    if args.runs % 6:
        parser.error("--runs must be a multiple of 6 so every tool order is used equally")
    binaries = {tool: getattr(args, tool).resolve() for tool in TOOLS}
    for tool, path in binaries.items():
        if not path.is_file():
            parser.error(f"{tool} binary does not exist: {path}")
    args.dataset_parent.mkdir(parents=True, exist_ok=True)
    if args.synthetic_root:
        base = args.synthetic_root.resolve()
        if os.name == "nt":  # the deep fixture is longer than MAX_PATH
            base = Path("\\\\?\\" + str(base))
        synthetic = [(n, base / n) for n in ("wide", "deep", "flat")]
    else:
        side = int(args.files ** 0.5)
        synthetic = make_trees(args.dataset_parent, wide=(side, args.files // side),
                               deep=(400, args.files // 400), flat=args.files)
    real = [(item.partition("=")[0], Path(item.partition("=")[2]).resolve()) for item in args.tree]
    result = dict(commit=args.commit, date=time.strftime("%Y-%m-%d"), cache="warm", runs=args.runs,
                  platform=platform.platform(), logical_cpus=os.cpu_count(), note=args.note,
                  complete=False,
                  tools={tool: dict(version=version(path), sha256=hashlib.sha256(path.read_bytes()).hexdigest())
                         for tool, path in binaries.items()}, datasets=[])
    scratch = Path(tempfile.mkdtemp(prefix="hyperdu-compare-", dir=args.dataset_parent))
    args.output.parent.mkdir(parents=True, exist_ok=True)
    for name, root in synthetic + real:
        # Synthetic files have no source extension, so tokei would only walk them: skip it there.
        tools = ("hyperdu", "dua") if (name, root) in synthetic else TOOLS
        measured = measure(binaries, name, root, args.runs, scratch, tools)
        result["datasets"].append(measured)
        args.output.write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
        print(name, measured["median_ms"], flush=True)
    for tool, path in binaries.items():
        if hashlib.sha256(path.read_bytes()).hexdigest() != result["tools"][tool]["sha256"]:
            raise RuntimeError(f"{tool} binary changed during benchmark")
    result["complete"] = True
    args.output.write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    shutil.rmtree(scratch, ignore_errors=True)


if __name__ == "__main__":
    main()
