#!/usr/bin/env python3
"""AWS EC2: identical allocated-byte accounting, alternating HyperDU/GNU du runs.

Read-only toward the dataset. No external adjustment to either tool's bytes.
Run correctness tests first. Build records and AWS instance/storage metadata are
supplied by the operator; this script additionally checks the local EC2 vendor.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import signal
import stat
import statistics
import subprocess
import sys
import time
from datetime import datetime, timezone
from pathlib import Path, PurePosixPath


def clean_environment(source: dict[str, str]) -> dict[str, str]:
    # Deliberately ignore caller settings, including loader and allocator overrides.
    return {"PATH": "/usr/bin:/bin", "LC_ALL": "C", "LANG": "C", "TZ": "UTC"}


def parse_rows(raw: bytes, root: Path) -> dict[str, int]:
    rows: dict[str, int] = {}
    root_name = root.as_posix()
    for line in raw.decode("utf-8", errors="strict").splitlines():
        number, separator, path = line.partition("\t")
        if not separator or not number.isdecimal() or not path:
            raise ValueError(f"Invalid bytes<TAB>path row: {line!r}")
        if path in rows:
            raise ValueError(f"Duplicate directory row: {path}")
        parsed = PurePosixPath(path)
        if (
            not parsed.is_absolute()
            or ".." in parsed.parts
            or not parsed.is_relative_to(PurePosixPath(root_name))
        ):
            raise ValueError(f"Output outside the measured root: {path}")
        rows[path] = int(number)
    if root_name not in rows:
        raise ValueError("Command did not report the root directory")
    return rows


def verify_equal(hyperdu: dict[str, int], du: dict[str, int]) -> None:
    if hyperdu != du:
        differing = [
            path
            for path in sorted(hyperdu.keys() | du.keys())
            if hyperdu.get(path) != du.get(path)
        ]
        examples = [(path, hyperdu.get(path), du.get(path)) for path in differing[:5]]
        raise ValueError(
            f"Direct directory-byte parity failed (path, hyperdu, du): {examples}"
        )


def fingerprint(root: Path) -> str:
    """Detect dataset mutation; metadata never adjusts the command results."""
    device = root.stat().st_dev
    digest = hashlib.sha256()
    stack = [root]
    while stack:
        path = stack.pop()
        info = path.lstat()
        row = (
            path.relative_to(root).as_posix(),
            info.st_dev,
            info.st_ino,
            info.st_mode,
            info.st_nlink,
            info.st_size,
            getattr(info, "st_blocks", None),
            info.st_mtime_ns,
            info.st_ctime_ns,
        )
        digest.update(json.dumps(row, ensure_ascii=True).encode("ascii") + b"\n")
        if stat.S_ISDIR(info.st_mode) and info.st_dev == device:
            with os.scandir(path) as entries:
                stack.extend(
                    sorted((Path(entry.path) for entry in entries), reverse=True)
                )
    return digest.hexdigest()


def file_hash(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


class RunFailure(RuntimeError):
    def __init__(self, command: list[str], reason: str, stdout: bytes, stderr: bytes):
        self.attempt = {
            "command": command,
            "reason": reason,
            "stdout": stdout.decode("utf-8", errors="replace"),
            "stderr": stderr.decode("utf-8", errors="replace"),
        }
        super().__init__(f"{reason}: {command!r}")


def run(
    command: list[str], env: dict[str, str], timeout: float
) -> tuple[float, bytes, str]:
    start = time.perf_counter_ns()
    with subprocess.Popen(
        command,
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        start_new_session=True,
    ) as process:
        try:
            stdout, stderr = process.communicate(timeout=timeout)
        except subprocess.TimeoutExpired:
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            stdout, stderr = process.communicate()
            raise RunFailure(command, "timeout", stdout, stderr)
        if process.returncode:
            raise RunFailure(command, f"exit={process.returncode}", stdout, stderr)
    elapsed = (time.perf_counter_ns() - start) / 1_000_000
    return elapsed, stdout, stderr.decode("utf-8", errors="replace")


def check_stderr(stderr: str, tool: str) -> None:
    # A successful exit with a scanner warning must not become a fast sample.
    if any(
        not (tool == "hyperdu" and line.startswith("fs-auto: "))
        for line in stderr.splitlines()
    ):
        raise ValueError(f"Unexpected {tool} diagnostic: {stderr}")


def save(path: Path, result: dict) -> None:
    temporary = path.with_suffix(path.suffix + ".partial")
    temporary.write_text(
        json.dumps(result, indent=2, ensure_ascii=True) + "\n", encoding="utf-8"
    )
    temporary.replace(path)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--scope", choices=("directory", "filesystem"), required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--build-record", type=Path, required=True)
    parser.add_argument("--aws-record", type=Path, required=True)
    parser.add_argument("--du", type=Path, default=Path("/usr/bin/du"))
    parser.add_argument("--du-record", type=Path, required=True)
    parser.add_argument("--source-snapshot", type=Path, required=True)
    parser.add_argument("--expected-head", required=True)
    parser.add_argument("--runs", type=int, default=8)
    parser.add_argument("--timeout", type=float, default=300)
    parser.add_argument(
        "--phase", choices=("pilot", "measurement"), default="measurement"
    )
    parser.add_argument("--threads", type=int)
    parser.add_argument(
        "--io-profile", choices=("balanced", "throughput"), default="balanced"
    )
    parser.add_argument("--prefetch", choices=("auto", "true", "false"), default="auto")
    parser.add_argument("--dir-yield-every", type=int, default=0)
    parser.add_argument(
        "--fs-auto", action=argparse.BooleanOptionalAction, default=True
    )
    args = parser.parse_args()
    if args.runs < 4 or args.runs % 2 or args.timeout <= 0:
        parser.error(
            "Require an even number of at least four runs and a positive timeout"
        )
    if args.phase == "measurement" and args.runs != 8:
        parser.error("The measurement phase requires exactly eight runs per tool")
    if (args.threads is not None and args.threads < 1) or args.dir_yield_every < 0:
        parser.error("Invalid threads or directory split interval")
    if platform.system() != "Linux" or platform.machine() != "x86_64":
        parser.error("This protocol is for Linux x86_64 on AWS EC2")
    vendor = Path("/sys/devices/virtual/dmi/id/sys_vendor").read_text().strip()
    if "Amazon EC2" not in vendor:
        parser.error(
            "AWS EC2 vendor check failed; local/WSL runs are not benchmark evidence"
        )
    binary, root, output = (
        args.binary.resolve(strict=True),
        args.root.resolve(strict=True),
        args.output.resolve(),
    )
    if not binary.is_file() or not root.is_dir():
        parser.error("Require a release executable and a directory root")
    if args.scope == "filesystem" and not root.is_mount():
        parser.error("Filesystem scope must be a mounted filesystem root")
    if output.exists() or output.with_suffix(output.suffix + ".partial").exists():
        parser.error("Choose a new output path; existing results are never overwritten")
    if output.is_relative_to(root) or binary.is_relative_to(root):
        parser.error("Keep results and executable outside the measured dataset")
    if (binary.parent / "hyperdu-config.json").exists():
        parser.error("Use an isolated executable directory without hyperdu-config.json")
    env = clean_environment(dict(os.environ))
    result = dict(
        schema_version=1,
        complete=False,
        stage="preflight",
        phase=args.phase,
        scope=args.scope,
        root=str(root),
        started_at=datetime.now(timezone.utc).isoformat(),
        cache="warm",
        accounting="allocated bytes, no link following, hardlink deduplication, one filesystem, all directory rows",
        external_byte_adjustment=False,
        runs=args.runs,
        timeout_seconds=args.timeout,
        effective_environment=env,
        samples=[],
        directory_totals={},
    )
    output.parent.mkdir(parents=True, exist_ok=True)
    save(output, result)
    try:
        du = args.du.resolve(strict=True)
        if not du.is_file() or not os.access(du, os.X_OK) or du.is_relative_to(root):
            raise ValueError(
                "GNU du must be an executable regular file outside the dataset"
            )
        du_record = json.loads(args.du_record.read_text(encoding="utf-8"))
        du_digest = file_hash(du)
        du_version = (
            run([str(du), "--version"], env, args.timeout)[1].decode().splitlines()[0]
        )
        if "GNU coreutils" not in du_version:
            raise ValueError("This protocol requires GNU du")
        if (
            du_record.get("binary_sha256", "").lower() != du_digest
            or du_record.get("version") != du_version
            or not du_record.get("package")
        ):
            raise ValueError("GNU du does not match its package provenance record")
        build_record = json.loads(args.build_record.read_text(encoding="utf-8"))
        aws_record = json.loads(args.aws_record.read_text(encoding="utf-8"))
        binary_digest = file_hash(binary)
        if build_record.get("binary_sha256", "").lower() != binary_digest:
            raise ValueError("Binary does not match the supplied build record")
        snapshot_digest = file_hash(args.source_snapshot)
        if (
            build_record.get("source_snapshot_sha256", "").lower() != snapshot_digest
            or build_record.get("head") != args.expected_head
            or not build_record.get("build_command")
            or not build_record.get("rustc_version")
        ):
            raise ValueError("Build source snapshot, HEAD or build provenance mismatch")
        if aws_record.get("provider") != "AWS":
            raise ValueError("AWS environment record is required")
        result.update(
            build_record=build_record,
            aws_record=aws_record,
            du_record=du_record,
            platform=platform.platform(),
            ec2_vendor=vendor,
            du_version=du_version,
            du_sha256=du_digest,
            binary_sha256=binary_digest,
            source_snapshot_sha256=snapshot_digest,
            expected_head=args.expected_head,
        )
        hd_command = [
            str(binary),
            "--compat",
            "gnu-strict",
            "--block-size",
            "1",
            "--one-file-system",
            "--io-profile",
            args.io_profile,
            "--dir-yield-every",
            str(args.dir_yield_every),
        ]
        if args.threads is not None:
            hd_command += ["--threads", str(args.threads)]
        if not args.fs_auto:
            hd_command += ["--no-fs-auto"]
        if args.prefetch != "auto":
            hd_command += [f"--prefetch={args.prefetch}"]
        hd_command += ["--", str(root)]
        commands = {
            "hyperdu": hd_command,
            "du": [str(du), "-x", "--block-size=1", "--", str(root)],
        }
        result["commands"] = commands
        result["stage"] = "parity"
        result["dataset_fingerprint"] = fingerprint(root)
        initial = {}
        for tool in ("du", "hyperdu"):
            result["active_attempt"] = {
                "tool": tool,
                "round": None,
                "command": commands[tool],
            }
            elapsed, raw, stderr = run(commands[tool], env, args.timeout)
            result["active_attempt"].update(
                elapsed_ms=elapsed,
                stdout_sha256=hashlib.sha256(raw).hexdigest(),
                stderr=stderr,
            )
            check_stderr(stderr, tool)
            initial[tool] = parse_rows(raw, root)
        verify_equal(initial["hyperdu"], initial["du"])
        result.pop("active_attempt", None)
        result["directory_totals"] = initial["du"]
        result["stage"] = "measurement"
        for iteration in range(args.runs):
            order = ("hyperdu", "du") if iteration % 2 == 0 else ("du", "hyperdu")
            for tool in order:
                result["active_attempt"] = {
                    "tool": tool,
                    "round": iteration,
                    "command": commands[tool],
                }
                elapsed, raw, stderr = run(commands[tool], env, args.timeout)
                result["active_attempt"].update(
                    elapsed_ms=elapsed,
                    stdout_sha256=hashlib.sha256(raw).hexdigest(),
                    stderr=stderr,
                )
                check_stderr(stderr, tool)
                rows = parse_rows(raw, root)
                verify_equal(rows, initial["du"])
                result["samples"].append(
                    dict(
                        round=iteration,
                        tool=tool,
                        elapsed_ms=elapsed,
                        stdout_sha256=hashlib.sha256(raw).hexdigest(),
                        stderr=stderr,
                        direct_directory_parity=True,
                    )
                )
                result.pop("active_attempt", None)
                save(output, result)
        if fingerprint(root) != result["dataset_fingerprint"]:
            raise ValueError("Dataset metadata changed during the measurement")
        if file_hash(binary) != binary_digest or file_hash(du) != result["du_sha256"]:
            raise ValueError("A measured executable changed during the measurement")
        result["median_ms"] = {
            tool: statistics.median(
                s["elapsed_ms"] for s in result["samples"] if s["tool"] == tool
            )
            for tool in ("hyperdu", "du")
        }
        result["du_over_hyperdu"] = (
            result["median_ms"]["du"] / result["median_ms"]["hyperdu"]
        )
        result["stage"] = "complete"
        result["complete"] = True
        result["finished_at"] = datetime.now(timezone.utc).isoformat()
        save(output, result)
        print(
            json.dumps(
                {
                    "median_ms": result["median_ms"],
                    "du_over_hyperdu": result["du_over_hyperdu"],
                }
            )
        )
        return 0
    except Exception as error:
        result["complete"] = False
        result["failed_stage"] = result["stage"]
        result["stage"] = "failed"
        if "active_attempt" in result:
            result["failed_attempt"] = result.pop("active_attempt")
        result["error"] = str(error)
        if isinstance(error, RunFailure):
            result.setdefault("failed_attempt", {}).update(error.attempt)
        result["finished_at"] = datetime.now(timezone.utc).isoformat()
        save(output, result)
        print(str(error), file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
