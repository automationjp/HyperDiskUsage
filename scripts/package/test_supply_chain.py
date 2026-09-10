"""Focused offline regression checks for release trust boundaries (requires PyYAML)."""

import hashlib
import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

import yaml

ROOT = Path(__file__).resolve().parents[2]
REV = "c535dd8b34e877ac93170ab941dccf020f6b3c2d"


class SupplyChainTests(unittest.TestCase):
    def test_actions_and_permissions(self):
        for path in (ROOT / ".github/workflows").glob("*.yml"):
            doc = yaml.safe_load(path.read_text())
            self.assertEqual(doc["permissions"], {"contents": "read"}, path)
            for name, job in doc["jobs"].items():
                for step in job.get("steps", []):
                    if "uses" in step:
                        self.assertRegex(step["uses"], r"^[^@]+@[0-9a-f]{40}$", path)
                privileged = any(
                    v == "write" for v in job.get("permissions", {}).values()
                )
                secret_job = "secrets." in str(job)
                if privileged or secret_job:
                    self.assertNotIn("actions/checkout@", str(job), name)
                    self.assertNotRegex(
                        str(job), r"cargo |scripts/package/|scripts/lint/", name
                    )
        release = yaml.safe_load((ROOT / ".github/workflows/release.yml").read_text())
        self.assertEqual(release["jobs"]["publish"]["needs"], ["linux", "windows"])
        self.assertEqual(
            release["jobs"]["publish"]["permissions"], {"contents": "write"}
        )
        for name in ("linux", "windows"):
            self.assertNotIn("secrets.", str(release["jobs"][name]))
            self.assertNotIn("action-gh-release", str(release["jobs"][name]))
        for name in ("scoop-pr", "winget-pr"):
            self.assertEqual(release["jobs"][name]["needs"], "publish")

    def test_release_collection_is_data_only(self):
        workflow = yaml.safe_load((ROOT / ".github/workflows/release.yml").read_text())
        command = next(
            step["run"]
            for step in workflow["jobs"]["publish"]["steps"]
            if step.get("name") == "Collect release files (data only)"
        )
        with tempfile.TemporaryDirectory() as td:
            base = Path(td)
            for folder in (
                "assets/linux/dist",
                "assets/windows",
                "assets/windows/scoop",
            ):
                (base / folder).mkdir(parents=True, exist_ok=True)
            (base / "assets/linux/dist/hyperdu-linux").write_text(
                "#!/bin/sh\ntouch executed\n"
            )
            (base / "assets/windows/hyperdu-windows.exe").write_bytes(b"binary")
            (base / "assets/linux/dist/build.log").write_text("private build log")
            (base / "assets/windows/scoop/hyperdu.json").write_text("{}")
            result = subprocess.run(
                ["bash", "-e", "-c", command], cwd=td, capture_output=True
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(
                {f.name for f in (base / "release-assets").iterdir()},
                {"hyperdu-linux", "hyperdu-windows.exe"},
            )
            self.assertFalse((base / "executed").exists())

    def test_appimage_invalid_tools_stop_before_build(self):
        with tempfile.TemporaryDirectory() as td:
            base = Path(td)
            cargo = base / "cargo"
            cargo.write_text('#!/bin/sh\ntouch "$MARKER"\n')
            cargo.chmod(0o700)
            env = dict(
                os.environ,
                PATH=f"{td}:/usr/bin:/bin",
                MARKER=str(base / "built"),
                LINUXDEPLOY="",
                APPIMAGETOOL="",
                LINUXDEPLOY_SHA256="",
                APPIMAGETOOL_SHA256="",
            )
            result = subprocess.run(
                ["bash", str(ROOT / "scripts/package/appimage.sh")],
                env=env,
                capture_output=True,
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertFalse((base / "built").exists())

    def test_no_mutable_downloads(self):
        for name in ("release.sh", "appimage.sh", "verified-appimage-tools.sh"):
            with self.subTest(script=name):
                source = (ROOT / "scripts/package" / name).read_text()
                # Guard both entry points and staging: alternate download tools
                # or mutable release URLs must not silently restore network fetches.
                self.assertNotRegex(
                    source, r"\b(?:curl|wget)\b|\bgh\s+release\s+download\b"
                )
                self.assertNotRegex(
                    source, r"/(?:download/continuous|releases/latest)(?:/|\b)"
                )
        self.assertIn(
            "verified-appimage-tools.sh",
            (ROOT / "scripts/package/appimage.sh").read_text(),
        )

    def test_tools_fail_closed_and_stage_only_verified_bytes(self):
        helper = ROOT / "scripts/package/verified-appimage-tools.sh"
        with tempfile.TemporaryDirectory() as td:
            base = Path(td)
            tool = base / "reviewed tool"
            tool.write_text('#!/bin/sh\necho NEVER_EXECUTE > "$MARKER"\n')
            digest = hashlib.sha256(tool.read_bytes()).hexdigest()
            env = dict(os.environ, TMPDIR=td, MARKER=str(base / "executed"))
            for hashes in [("", digest), ("0" * 64, digest), (digest, "0" * 64)]:
                result = subprocess.run(
                    ["bash", str(helper), str(tool), hashes[0], str(tool), hashes[1]],
                    env=env,
                    capture_output=True,
                    text=True,
                )
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse(list(base.glob("hyperdu-appimage.*")))
                self.assertEqual(tool.stat().st_mode & 0o111, 0)
            # A preinstalled executable is subject to the same check and is
            # never overwritten or chmodded, even when its digest is wrong.
            tool.chmod(0o700)
            before = tool.read_bytes()
            rejected = subprocess.run(
                ["bash", str(helper), str(tool), "0" * 64, str(tool), digest],
                env=env,
                capture_output=True,
            )
            self.assertNotEqual(rejected.returncode, 0)
            self.assertEqual(tool.read_bytes(), before)
            self.assertEqual(tool.stat().st_mode & 0o777, 0o700)
            self.assertFalse(list(base.glob("hyperdu-appimage.*")))
            result = subprocess.run(
                ["bash", str(helper), str(tool), digest, str(tool), digest],
                env=env,
                capture_output=True,
                text=True,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            staged = Path(result.stdout.strip())
            try:
                for name in ("linuxdeploy", "appimagetool"):
                    self.assertEqual((staged / name).read_bytes(), tool.read_bytes())
                    self.assertTrue((staged / name).stat().st_mode & 0o100)
                self.assertFalse((base / "executed").exists())
            finally:
                shutil.rmtree(staged)

    def test_plugin_remote_rev_and_local_path(self):
        source = ROOT / "plugin/skills/disk-space-triage/scripts"
        self.assertIn("--rev $SourceRev", (source / "setup-hyperdu.ps1").read_text())
        self.assertIn(REV, (source / "setup-hyperdu.ps1").read_text())
        for local in (False, True):
            with tempfile.TemporaryDirectory() as td:
                base = Path(td)
                scripts = base / "plugin/skills/disk-space-triage/scripts"
                scripts.mkdir(parents=True)
                shutil.copy(source / "setup-hyperdu.sh", scripts)
                if local:
                    (base / "hyperdu").mkdir()
                    (base / "hyperdu/Cargo.toml").touch()
                bins = base / "bin"
                bins.mkdir()
                cargo = bins / "cargo"
                cargo.write_text(
                    '#!/bin/sh\nprintf "%s\\n" "$@" > "$ARGS"\ntouch "$INSTALLED"\n'
                )
                hyperdu = bins / "hyperdu"
                hyperdu.write_text(
                    '#!/bin/sh\n[ -f "$INSTALLED" ] || exit 1\necho "Usage: hyperdu mcp"\n'
                )
                cargo.chmod(0o700)
                hyperdu.chmod(0o700)
                env = dict(
                    os.environ,
                    PATH=f"{bins}:/usr/bin:/bin",
                    ARGS=str(base / "args"),
                    INSTALLED=str(base / "installed"),
                )
                result = subprocess.run(
                    ["sh", str(scripts / "setup-hyperdu.sh")],
                    env=env,
                    capture_output=True,
                    text=True,
                )
                self.assertEqual(result.returncode, 0, result.stderr)
                args = (base / "args").read_text().splitlines()
                self.assertEqual(args[:3], ["install", "--locked", "--force"])
                if local:
                    self.assertEqual(args[3:], ["--path", str(base / "hyperdu")])
                else:
                    self.assertEqual(
                        args[3:],
                        [
                            "--git",
                            "https://github.com/automationjp/HyperDiskUsage",
                            "--rev",
                            REV,
                            "hyperdu",
                        ],
                    )


if __name__ == "__main__":
    unittest.main()
