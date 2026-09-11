"""Contract tests for the AWS du benchmark gate; these are not timings."""

import importlib.util
import json
import os
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


class GithubEnvironmentGateTests(unittest.TestCase):
    def setUp(self):
        self.environment = {
            "GITHUB_ACTIONS": "true",
            "RUNNER_ENVIRONMENT": "github-hosted",
            "RUNNER_OS": "Linux",
            "RUNNER_ARCH": "X64",
            "GITHUB_SHA": "abc123",
            "GITHUB_REPOSITORY": "automationjp/HyperDiskUsage",
            "GITHUB_RUN_ID": "12345",
            "GITHUB_RUN_ATTEMPT": "2",
            "GITHUB_JOB": "product-benchmark",
        }
        self.record = {
            "provider": "GitHub Actions",
            "head": "abc123",
            "repository": "automationjp/HyperDiskUsage",
            "run_id": "12345",
            "run_attempt": "2",
            "job": "product-benchmark",
            "run_url": "https://github.com/automationjp/HyperDiskUsage/actions/runs/12345",
        }

    def validate(self, environment=None, record=None, **observed):
        return bench.validate_github_actions_environment(
            environment or self.environment,
            record or self.record,
            "abc123",
            kernel_release=observed.get("kernel_release", "6.8.0-1018-azure"),
            kernel_version=observed.get("kernel_version", "#1 SMP"),
            dmi_vendor=observed.get("dmi_vendor", "Microsoft Corporation"),
        )

    def test_valid_github_record_captures_observed_runner_metadata(self):
        result = self.validate()
        self.assertIsNot(result, self.record)
        self.assertEqual(result["provider"], "GitHub Actions")
        self.assertEqual(result["observed_dmi_vendor"], "Microsoft Corporation")
        self.assertEqual(result["observed_kernel_release"], "6.8.0-1018-azure")
        self.assertEqual(result["observed_kernel_version"], "#1 SMP")

    def test_wsl_kernel_is_rejected(self):
        with self.assertRaisesRegex(ValueError, "WSL"):
            self.validate(kernel_release="5.15.153.1-microsoft-standard-WSL2")

    def test_local_and_self_hosted_runners_are_rejected(self):
        local = dict(self.environment)
        local.pop("GITHUB_ACTIONS")
        with self.assertRaisesRegex(ValueError, "GITHUB_ACTIONS"):
            self.validate(environment=local)

        self_hosted = dict(self.environment, RUNNER_ENVIRONMENT="self-hosted")
        with self.assertRaisesRegex(ValueError, "RUNNER_ENVIRONMENT"):
            self.validate(environment=self_hosted)

    def test_head_and_run_provenance_mismatches_are_rejected(self):
        with self.assertRaisesRegex(ValueError, "head"):
            self.validate(record=dict(self.record, head="wrong-head"))
        with self.assertRaisesRegex(ValueError, "run_id"):
            self.validate(record=dict(self.record, run_id="99999"))


class ArgumentValidationTests(unittest.TestCase):
    def test_environment_record_arguments_are_conditional(self):
        common = [
            "--binary",
            "binary",
            "--root",
            "root",
            "--scope",
            "directory",
            "--output",
            "output",
            "--build-record",
            "build",
            "--du-record",
            "du",
            "--source-snapshot",
            "snapshot",
            "--expected-head",
            "head",
        ]
        with patch.object(
            sys,
            "argv",
            ["bench", *common, "--environment", "aws"],
        ), self.assertRaises(SystemExit) as aws_error:
            bench.main()
        self.assertEqual(aws_error.exception.code, 2)

        with patch.object(
            sys,
            "argv",
            ["bench", *common, "--environment", "github-actions"],
        ), self.assertRaises(SystemExit) as github_error:
            bench.main()
        self.assertEqual(github_error.exception.code, 2)


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
        self.github_environment = {
            "GITHUB_ACTIONS": "true",
            "RUNNER_ENVIRONMENT": "github-hosted",
            "RUNNER_OS": "Linux",
            "RUNNER_ARCH": "X64",
            "GITHUB_SHA": "test-head",
            "GITHUB_REPOSITORY": "automationjp/HyperDiskUsage",
            "GITHUB_RUN_ID": "12345",
            "GITHUB_RUN_ATTEMPT": "1",
            "GITHUB_JOB": "benchmark",
        }
        self.github_record = {
            "provider": "GitHub Actions",
            "head": "test-head",
            "repository": "automationjp/HyperDiskUsage",
            "run_id": "12345",
            "run_attempt": "1",
            "job": "benchmark",
            "run_url": "https://github.com/automationjp/HyperDiskUsage/actions/runs/12345",
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

    def execute(
        self,
        *,
        mismatch=False,
        mutate=False,
        timeout=False,
        warning=False,
        environment="aws",
        source_mismatch=False,
    ):
        argv = list(self.argv)
        if environment == "github-actions":
            record_flag = argv.index("--aws-record")
            argv[record_flag] = "--github-record"
            argv[record_flag + 1] = str(self.base / "github.json")
            argv[record_flag:record_flag] = ["--environment", "github-actions"]
        build = dict(self.build)
        if source_mismatch:
            build["source_snapshot_sha256"] = "bad"
        for name, record in [
            ("build", build),
            ("du", self.du_record),
            (
                "aws" if environment == "aws" else "github",
                {"provider": "AWS"} if environment == "aws" else self.github_record,
            ),
        ]:
            (self.base / f"{name}.json").write_text(
                json.dumps(record), encoding="utf-8"
            )
        original_read = Path.read_text

        def read(path, *args, **kwargs):
            if str(path) == "/sys/devices/virtual/dmi/id/sys_vendor":
                return (
                    "Amazon EC2"
                    if environment == "aws"
                    else "Microsoft Corporation"
                )
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
            patch.object(sys, "argv", argv),
            patch.object(bench.platform, "system", return_value="Linux"),
            patch.object(bench.platform, "machine", return_value="x86_64"),
            patch.object(bench.platform, "release", return_value="6.8.0-1018-azure"),
            patch.object(bench.platform, "version", return_value="#1 SMP"),
            patch.object(Path, "read_text", read),
            patch.object(bench, "run", side_effect=run),
            patch.object(
                bench,
                "fingerprint",
                side_effect=["before", "after" if mutate else "before"],
            ),
            patch.dict(
                os.environ,
                self.github_environment if environment == "github-actions" else {},
                clear=False,
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

    def test_github_mode_receipt_keeps_runner_record_and_no_ec2_label(self):
        self.assertEqual(self.execute(environment="github-actions"), 0)
        receipt = json.loads(self.output.read_text())
        self.assertTrue(receipt["complete"])
        self.assertEqual(receipt["environment"], "github-actions")
        self.assertEqual(receipt["github_record"], {
            **self.github_record,
            "observed_dmi_vendor": "Microsoft Corporation",
            "observed_kernel_release": "6.8.0-1018-azure",
            "observed_kernel_version": "#1 SMP",
        })
        self.assertNotIn("aws_record", receipt)
        self.assertNotIn("ec2_vendor", receipt)

    def test_github_source_proof_mismatch_fails_before_measurement(self):
        self.assertEqual(
            self.execute(environment="github-actions", source_mismatch=True),
            1,
        )
        receipt = json.loads(self.output.read_text())
        self.assertFalse(receipt["complete"])
        self.assertEqual(receipt["failed_stage"], "preflight")
        self.assertEqual(self.calls, [])

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
