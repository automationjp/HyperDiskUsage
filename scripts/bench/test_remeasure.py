"""Behavioral contracts for the shared corpus benchmark, including real child telemetry."""
import importlib.util
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parent))
spec = importlib.util.spec_from_file_location("remeasure", Path(__file__).with_name("remeasure.py"))
bench = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bench)


class CorpusTests(unittest.TestCase):
    def test_shapes_have_equal_file_workloads_and_distinct_depth(self):
        with tempfile.TemporaryDirectory() as temporary:
            parent = Path(temporary)
            results = [bench.oracle(bench.make_tree(parent, shape, 257, 8, 37))
                       for shape in ("flat", "wide", "deep")]
            for result in results:
                self.assertEqual(result["root"][0], 257 * 37)
                self.assertEqual(result["root"][2], 257)
            self.assertEqual(results[0]["directories"], 1)
            self.assertGreater(results[2]["directories"], results[1]["directories"])

    def test_oracle_counts_hardlink_once_and_detects_mutation(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "a").write_bytes(b"12345")
            os.link(root / "a", root / "b")
            before = bench.oracle(root)
            self.assertEqual(before["root"][0::2], [5, 1])
            self.assertEqual(before["entries"], 2)
            (root / "a").write_bytes(b"123456")
            after = bench.oracle(root)
            self.assertNotEqual(before["digest"], after["digest"])
            self.assertEqual(after["root"][0], 6)

    def test_report_rejects_duplicates_wrong_counts_and_outside_paths(self):
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary)
            root = base / "root"
            root.mkdir()
            report = base / "rows.json"
            row = dict(path=str(root), logical=1, physical=4096, files=1)
            report.write_text(json.dumps([row, row]))
            with self.assertRaises(ValueError):
                bench.parse_report(report, root)
            report.write_text(json.dumps([dict(row, path=str(base / "outside"))]))
            with self.assertRaises(ValueError):
                bench.parse_report(report, root)
            report.write_text(json.dumps([dict(row, logical=True)]))
            with self.assertRaises(ValueError):
                bench.parse_report(report, root)
            with self.assertRaisesRegex(ValueError, "parity"):
                bench.verify_rows({".": [1, 4096, 1]}, {".": [1, 0, 1]})

    def test_overrides_cannot_change_accounting_flags(self):
        for flags in ('["--apparent-size"]', '["--follow-links"]', '["--threads"]'):
            with self.assertRaises(ValueError):
                bench.parse_flags(flags)
        with self.assertRaises(ValueError):
            bench.parse_json_map('{"LD_PRELOAD":"library"}')
        self.assertEqual(bench.parse_flags('["--threads","2","--mft"]'),
                         ["--threads", "2", "--mft"])

    def test_setup_rejects_source_and_input_overlaps_before_writes(self):
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary)
            source, root = base / "source", base / "input"
            source.mkdir()
            root.mkdir()
            for output, parent in (
                (source / "out.json", base / "data"),
                (base / "out.json", source / "data"),
                (root / "out.json", base / "data"),
                (base / "out.json", root / "data"),
            ):
                with self.assertRaisesRegex(ValueError, "outside"):
                    bench.validate_locations(source, output, parent, [root])
                self.assertFalse(output.exists())
                self.assertFalse(parent.exists())
            bench.validate_locations(source, base / "out.json", base / "data", [root])

    @unittest.skipUnless(os.name != "nt", "POSIX symlink fixture")
    def test_oracle_entry_denominator_excludes_skipped_descendants(self):
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary)
            root, target = base / "root", base / "outside"
            root.mkdir()
            target.mkdir()
            (target / "not-traversed").write_bytes(b"data")
            (root / "link").symlink_to(target, target_is_directory=True)
            result = bench.oracle(root)
            self.assertEqual(result["entries"], 1)
            self.assertEqual(result["directories"], 1)
            self.assertEqual(result["root"], [0, 0, 0])
    def test_strace_total_handles_optional_error_column(self):
        self.assertEqual(bench.syscall_count("100.00  0.01  3  567  4 total\n"), 567)
        self.assertEqual(bench.syscall_count("100.00  0.01  3  567 total\n"), 567)
        with self.assertRaises(ValueError):
            bench.syscall_count("strace: permission denied")


class ChildTests(unittest.TestCase):
    def test_real_child_has_final_cpu_memory_and_byte_units(self):
        metrics, stdout, stderr = bench.measure_process(
            [sys.executable, "-c", "x=bytearray(4*1024*1024); print(sum(x))"],
            bench.clean_environment(), 10)
        self.assertEqual(stdout.strip(), b"0")
        self.assertEqual(stderr, b"")
        self.assertGreater(metrics["wall_seconds"], 0)
        self.assertGreater(metrics["peak_rss_bytes"], 4 * 1024 * 1024)
        self.assertGreaterEqual(metrics["cpu_seconds"], 0)
        self.assertGreaterEqual(metrics["page_faults"], 0)
        if os.name == "nt":
            self.assertIsNotNone(metrics["bytes_read"])
        elif metrics["bytes_read"] is None:
            self.assertIn("unavailable:", metrics["bytes_read_method"])
            self.assertGreaterEqual(metrics["input_blocks"], 0)
        else:
            self.assertEqual(sys.platform, "linux")
            self.assertGreaterEqual(metrics["bytes_read"], 0)

    @unittest.skipUnless(sys.platform == "linux", "Linux proc accounting")
    def test_unreadable_final_io_keeps_other_metrics_and_explains_missing_counter(self):
        with patch.object(bench.Path, "read_text",
                          side_effect=PermissionError(13, "injected proc permission")):
            metrics, stdout, stderr = bench.measure_process(
                [sys.executable, "-c", "print('finished')"], bench.clean_environment(), 10)
        self.assertEqual(stdout.strip(), b"finished")
        self.assertEqual(stderr, b"")
        self.assertIsNone(metrics["bytes_read"])
        self.assertIn("PermissionError errno=13", metrics["bytes_read_method"])
        self.assertGreater(metrics["peak_rss_bytes"], 0)
        self.assertGreaterEqual(metrics["cpu_seconds"], 0)

    @unittest.skipUnless(sys.platform == "linux" and bench.shutil.which("strace"),
                         "requires Linux strace")
    def test_real_threaded_child_syscalls_are_not_scanner_warnings(self):
        with tempfile.TemporaryDirectory() as temporary:
            probe = bench.syscall_probe(
                [sys.executable, "-c",
                 "import threading; t=threading.Thread(target=lambda: None); t.start(); t.join()"],
                bench.clean_environment(), 10, Path(temporary))
        self.assertGreater(probe["count"], 0)
    def test_failed_child_is_not_a_fast_sample(self):
        with self.assertRaisesRegex(RuntimeError, "exit=7"):
            bench.measure_process([sys.executable, "-c", "raise SystemExit(7)"],
                                  bench.clean_environment(), 10)

    def test_timeout_terminates_child(self):
        with self.assertRaises(Exception) as raised:
            bench.measure_process([sys.executable, "-c", "import time; time.sleep(60)"],
                                  bench.clean_environment(), 0.05)
        self.assertIsInstance(raised.exception, bench.subprocess.TimeoutExpired)


class TransactionTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.base = Path(self.temporary.name)
        self.binary = self.base / "hyperdu"
        self.binary.write_bytes(b"fixture")
        self.tree = bench.make_tree(self.base, "flat", 3, 2, 7)
        self.output = self.base / "result.json"
        self.build_record = self.base / "build.json"
        self.build_record.write_text(json.dumps(dict(head="test",
            binary_sha256=bench.file_hash(self.binary), build_command="fixture build")))
        self.args = [
            "remeasure.py", "--bin", str(self.binary), "--commit", "test",
            "--build-record", str(self.build_record),
            "--output", str(self.output), "--dataset-parent", str(self.base),
            "--tree", str(self.tree), "--baseline-env", '{"HYPERDU_FS_AUTO":"0"}',
            "--environment-label", "unit-fixture", "--runs", "4", "--syscalls", "off"]
        self.calls = 0

    def scan(self, command, env, timeout):
        self.calls += 1
        root = Path(command[1])
        rows = bench.oracle(root)["rows"]
        output = Path(command[command.index("--json") + 1])
        output.write_text(json.dumps([
            dict(path=str(root / key), logical=row[0], physical=row[1], files=row[2])
            for key, row in rows.items()]))
        return dict(wall_seconds=0.01, command=command)

    def execute(self, scanner=None):
        with patch.object(sys, "argv", self.args), \
             patch.object(bench, "source_record", return_value={"fixture": True, "clean": True}), \
             patch.object(bench, "checked_scan", side_effect=scanner or self.scan):
            return bench.main()

    def test_balanced_trials_complete_only_after_all_parity_gates(self):
        self.assertEqual(self.execute(), 0)
        result = json.loads(self.output.read_text())
        self.assertTrue(result["complete"])
        self.assertEqual(self.calls, 10)  # Two warm-ups, four measured pairs.
        self.assertEqual([sample["tool"] for sample in result["datasets"][0]["samples"]],
                         ["candidate", "baseline", "baseline", "candidate"] * 2)

    def test_mid_measurement_failure_retains_incomplete_result(self):
        def fail(command, env, timeout):
            if self.calls == 4:
                raise ValueError("injected parity failure")
            return self.scan(command, env, timeout)
        with self.assertRaisesRegex(ValueError, "injected"):
            self.execute(fail)
        result = json.loads(self.output.read_text())
        self.assertFalse(result["complete"])
        self.assertIn("injected", result["error"])
        self.assertEqual(len(result["datasets"][0]["samples"]), 2)

    def test_dirty_source_is_rejected_before_creating_output(self):
        with patch.object(sys, "argv", self.args), \
             patch.object(bench, "source_record", return_value={"clean": False}):
            with self.assertRaisesRegex(ValueError, "clean committed"):
                bench.main()
        self.assertFalse(self.output.exists())
    def test_existing_result_is_never_overwritten(self):
        self.output.write_text("keep")
        with self.assertRaises(SystemExit):
            self.execute()
        self.assertEqual(self.output.read_text(), "keep")

    def test_cold_requires_a_reset_for_each_trial(self):
        self.args += ["--cache", "cold"]
        with self.assertRaises(SystemExit):
            self.execute()
        self.args += ["--cache-reset", '["cache-reset-fixture"]']
        with patch.object(bench, "measure_process", return_value=({}, b"", b"")) as reset:
            self.assertEqual(self.execute(), 0)
        self.assertEqual(reset.call_count, 8)
        self.assertEqual(len(json.loads(self.output.read_text())["datasets"][0]["cache_resets"]), 8)


if __name__ == "__main__":
    unittest.main()
