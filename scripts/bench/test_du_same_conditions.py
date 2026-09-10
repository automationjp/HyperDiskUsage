"""Contract tests for the AWS du benchmark gate; these are not timings."""

import importlib.util
import json
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

spec = importlib.util.spec_from_file_location(
    "du_same_conditions", Path(__file__).with_name("du_same_conditions.py")
)
assert spec is not None and spec.loader is not None
bench = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bench)


class AccountingGateTests(unittest.TestCase):
    def test_parses_direct_directory_bytes(self):
        root = Path("/fixture")
        self.assertEqual(
            bench.parse_rows(b"8192\t/fixture\n4096\t/fixture/empty\n", root),
            {"/fixture": 8192, "/fixture/empty": 4096},
        )

    def test_missing_root_and_duplicates_are_rejected(self):
        for raw in [
            b"4096\t/fixture/empty\n",
            b"1\t/fixture\n1\t/fixture\n",
            b"-1\t/fixture\n",
            b"1\t/fixture\n1\t/fixture/../outside\n",
        ]:
            with self.subTest(raw=raw), self.assertRaises(ValueError):
                bench.parse_rows(raw, Path("/fixture"))

    def test_no_external_adjustment_can_make_different_totals_pass(self):
        with self.assertRaises(ValueError):
            bench.verify_equal({"/fixture": 4096}, {"/fixture": 8192})
        with self.assertRaises(ValueError):
            bench.verify_equal(
                {"/fixture": 8192}, {"/fixture": 8192, "/fixture/empty": 0}
            )
        bench.verify_equal({"/fixture": 8192}, {"/fixture": 8192})

    def test_metadata_fingerprint_detects_dataset_changes(self):
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            (root / "file").write_bytes(b"before")
            before = bench.fingerprint(root)
            self.assertEqual(before, bench.fingerprint(root))
            (root / "file").write_bytes(b"different contents")
            self.assertNotEqual(before, bench.fingerprint(root))

    def test_environment_does_not_inherit_scan_or_unit_overrides(self):
        env = bench.clean_environment(
            {
                "PATH": "/bin",
                "HYPERDU_FS_AUTO": "0",
                "BLOCK_SIZE": "512",
                "RUST_LOG": "debug",
            }
        )
        self.assertEqual(env["PATH"], "/usr/bin:/bin")
        self.assertEqual(env.get("LC_ALL"), "C")
        self.assertNotIn("HYPERDU_FS_AUTO", env)
        self.assertNotIn("BLOCK_SIZE", env)
        self.assertNotIn("RUST_LOG", env)


@unittest.skipUnless(sys.platform == "linux", "The benchmark transaction is Linux-only")
class MeasurementTransactionTests(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory()
        self.addCleanup(self.scratch.cleanup)
        self.base = Path(self.scratch.name)
        self.root = self.base / "fixture"
        self.root.mkdir()
        self.binary = self.base / "hyperdu"
        self.binary.write_bytes(b"isolated test executable")
        self.du = self.base / "du"
        self.du.write_bytes(b"isolated test oracle")
        self.du.chmod(0o700)
        self.snapshot = self.base / "source.json"
        self.snapshot.write_text('{"test_fixture":true}', encoding="utf-8")
        self.version = "du (GNU coreutils) 9.4"
        self.build = {
            "binary_sha256": bench.file_hash(self.binary),
            "source_snapshot_sha256": bench.file_hash(self.snapshot),
            "head": "test-head",
            "build_command": "test build",
            "rustc_version": "test",
        }
        self.du_record = {
            "binary_sha256": bench.file_hash(self.du),
            "version": self.version,
            "package": "test coreutils",
        }
        self.output = self.base / "result.json"
        self.argv = [
            "bench",
            "--binary",
            str(self.binary),
            "--du",
            str(self.du),
            "--root",
            str(self.root),
            "--scope",
            "directory",
            "--output",
            str(self.output),
            "--build-record",
            str(self.base / "build.json"),
            "--aws-record",
            str(self.base / "aws.json"),
            "--du-record",
            str(self.base / "du.json"),
            "--source-snapshot",
            str(self.snapshot),
            "--expected-head",
            "test-head",
        ]
        self.calls = []

    def execute(self, *, mismatch=False, mutate=False, timeout=False, warning=False):
        for name, record in [
            ("build", self.build),
            ("du", self.du_record),
            ("aws", {"provider": "AWS"}),
        ]:
            (self.base / f"{name}.json").write_text(
                json.dumps(record), encoding="utf-8"
            )
        original_read = Path.read_text

        def read(path, *args, **kwargs):
            if str(path) == "/sys/devices/virtual/dmi/id/sys_vendor":
                return "Amazon EC2"
            return original_read(path, *args, **kwargs)

        def run(command, env, limit):
            self.assertEqual(
                env, {"PATH": "/usr/bin:/bin", "LC_ALL": "C", "LANG": "C", "TZ": "UTC"}
            )
            if command[-1] == "--version":
                return 1.0, self.version.encode(), ""
            self.calls.append(command)
            if timeout:
                raise bench.RunFailure(command, "timeout", b"partial", b"diagnostic")
            amount = 8192 if mismatch and command[0] == str(self.binary) else 4096
            return (
                1.0,
                f"{amount}\t{self.root.as_posix()}\n".encode(),
                "warning" if warning else "",
            )

        with (
            patch.object(sys, "argv", self.argv),
            patch.object(bench.platform, "system", return_value="Linux"),
            patch.object(bench.platform, "machine", return_value="x86_64"),
            patch.object(Path, "read_text", read),
            patch.object(bench, "run", side_effect=run),
            patch.object(
                bench,
                "fingerprint",
                side_effect=["before", "after" if mutate else "before"],
            ),
        ):
            return bench.main()

    def test_success_has_eight_alternating_pairs_and_exact_commands(self):
        self.assertEqual(self.execute(), 0)
        receipt = json.loads(self.output.read_text())
        self.assertTrue(receipt["complete"])
        self.assertEqual(len(receipt["samples"]), 16)
        self.assertEqual(
            [s["tool"] for s in receipt["samples"]],
            [
                tool
                for round_number in range(8)
                for tool in (
                    ("hyperdu", "du") if round_number % 2 == 0 else ("du", "hyperdu")
                )
            ],
        )
        self.assertEqual(
            receipt["commands"]["du"],
            [str(self.du), "-x", "--block-size=1", "--", str(self.root)],
        )
        self.assertEqual(
            receipt["commands"]["hyperdu"],
            [
                str(self.binary),
                "--compat",
                "gnu-strict",
                "--block-size",
                "1",
                "--one-file-system",
                "--io-profile",
                "balanced",
                "--dir-yield-every",
                "0",
                "--",
                str(self.root),
            ],
        )

    def test_performance_options_are_recorded_in_command(self):
        self.argv += [
            "--threads",
            "4",
            "--io-profile",
            "throughput",
            "--prefetch",
            "false",
            "--dir-yield-every",
            "8",
            "--no-fs-auto",
        ]
        self.assertEqual(self.execute(), 0)
        command = json.loads(self.output.read_text())["commands"]["hyperdu"]
        for flag, value in [
            ("--threads", "4"),
            ("--io-profile", "throughput"),
            ("--dir-yield-every", "8"),
        ]:
            self.assertEqual(command[command.index(flag) + 1], value)
        self.assertIn("--prefetch=false", command)
        self.assertIn("--no-fs-auto", command)

    def test_final_print_failure_invalidates_receipt(self):
        with patch(
            "builtins.print", side_effect=[BrokenPipeError("closed output"), None]
        ):
            self.assertEqual(self.execute(), 1)
        self.assertFalse(json.loads(self.output.read_text())["complete"])

    def test_wrong_run_count_is_rejected(self):
        self.argv += ["--runs", "4"]
        with self.assertRaises(SystemExit) as error:
            self.execute()
        self.assertEqual(error.exception.code, 2)
        self.assertFalse(self.output.exists())

    def test_parity_failure_is_saved_without_valid_samples(self):
        self.assertEqual(self.execute(mismatch=True), 1)
        receipt = json.loads(self.output.read_text())
        self.assertFalse(receipt["complete"])
        self.assertIn("parity failed", receipt["error"])
        self.assertEqual(receipt["samples"], [])

    def test_mutation_invalidates_all_samples(self):
        self.assertEqual(self.execute(mutate=True), 1)
        receipt = json.loads(self.output.read_text())
        self.assertFalse(receipt["complete"])
        self.assertEqual(len(receipt["samples"]), 16)
        self.assertNotIn("median_ms", receipt)

    def test_build_mismatch_is_saved_during_preflight(self):
        self.build["head"] = "stale-head"
        self.assertEqual(self.execute(), 1)
        receipt = json.loads(self.output.read_text())
        self.assertFalse(receipt["complete"])
        self.assertEqual(receipt["failed_stage"], "preflight")
        self.assertEqual(self.calls, [])

    def test_oracle_mismatch_is_saved_during_preflight(self):
        self.du_record["binary_sha256"] = "bad"
        self.assertEqual(self.execute(), 1)
        self.assertFalse(json.loads(self.output.read_text())["complete"])
        self.assertEqual(self.calls, [])

    def test_timeout_saves_failed_attempt(self):
        self.assertEqual(self.execute(timeout=True), 1)
        receipt = json.loads(self.output.read_text())
        self.assertFalse(receipt["complete"])
        self.assertEqual(receipt["failed_attempt"]["reason"], "timeout")
        self.assertEqual(receipt["failed_attempt"]["stdout"], "partial")

    def test_scanner_warning_invalidates_run(self):
        self.assertEqual(self.execute(warning=True), 1)
        self.assertFalse(json.loads(self.output.read_text())["complete"])


class ProcessTimeoutTests(unittest.TestCase):
    def test_timeout_kills_session_and_drains_diagnostics(self):
        with (
            patch.object(bench.subprocess, "Popen") as popen,
            patch.object(bench.os, "killpg", create=True) as kill,
        ):
            process = popen.return_value.__enter__.return_value
            process.pid = 123
            process.communicate.side_effect = [
                bench.subprocess.TimeoutExpired(["fake"], 1),
                (b"partial", b"error"),
            ]
            # SIGKILL is absent on Windows; the production runner is Linux-only.
            with (
                patch.object(bench.signal, "SIGKILL", 9, create=True),
                self.assertRaises(bench.RunFailure),
            ):
                bench.run(["fake"], {}, 1)
            kill.assert_called_once_with(123, 9)
            self.assertTrue(popen.call_args.kwargs["start_new_session"])
            self.assertEqual(process.communicate.call_count, 2)


if __name__ == "__main__":
    unittest.main()
