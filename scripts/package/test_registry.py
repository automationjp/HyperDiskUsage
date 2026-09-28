"""Offline tests of the real registry workflow; never use credentials or a network."""

import contextlib
import hashlib
import importlib.util
import io
import json
import os
import struct
import tarfile
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch
from urllib.error import HTTPError

import yaml

ROOT = Path(__file__).resolve().parents[2]
VERSION = "0.5.0-beta.5"
SPEC = importlib.util.spec_from_file_location(
    "prepare_registry", ROOT / "scripts/package/prepare_registry.py"
)
PREPARE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PREPARE)


def uploader():
    workflow = yaml.safe_load((ROOT / ".github/workflows/registry.yml").read_text())
    command = workflow["jobs"]["upload"]["steps"][-1]["run"]
    source = command.split("python3 - <<'PY'\n", 1)[1].rsplit("\nPY", 1)[0]
    namespace = {"__name__": "registry_test"}
    exec(compile(source, "registry-workflow", "exec"), namespace)
    return namespace


def crate(path, name="hyperdu", extra="", readme=True, duplicate=False):
    manifest = (f'[package]\nname = "{name}"\nversion = "{VERSION}"\n'
                'edition = "2021"\nlicense = "MIT"\n'
                + ('readme = "README.md"\n' if readme else "") + extra)
    with tarfile.open(path, "w:gz") as archive:
        for member_name, data in [("Cargo.toml", manifest), ("README.md", "概要")]:
            entry = tarfile.TarInfo(f"{name}-{VERSION}/{member_name}")
            body = data.encode()
            entry.size = len(body)
            archive.addfile(entry, io.BytesIO(body))
            if duplicate and member_name == "Cargo.toml":
                archive.addfile(entry, io.BytesIO(body))


def payload(name):
    meta = json.dumps({"name": name, "vers": VERSION}).encode()
    body = b"not executed: " + name.encode()
    return struct.pack("<I", len(meta)) + meta + struct.pack("<I", len(body)) + body


class FakeRegistry:
    def __init__(self, existing=False, error=None, reject=False, wrong=False, yanked=False):
        self.calls = []
        self.existing = existing
        self.error = error
        self.reject = reject
        self.wrong = wrong
        self.yanked = yanked

    def open(self, req, timeout):
        self.calls.append(req)
        if self.error:
            raise HTTPError(req.full_url, self.error, "test failure", {}, None)
        if req.method == "PUT":
            return contextlib.closing(io.BytesIO(json.dumps(
                {"errors": [{"detail": "rejected"}]} if self.reject else {"ok": True}
            ).encode()))
        if "index.crates.io" in req.full_url:
            name = req.full_url.rsplit("/", 1)[-1]
        else:
            if not self.existing:
                raise HTTPError(req.full_url, 404, "not found", {}, None)
            name = req.full_url.rsplit("/", 2)[-2]
        checksum = hashlib.sha256(b"not executed: " + name.encode()).hexdigest()
        if self.wrong:
            checksum = "0" * 64
        row = {"vers": VERSION, "cksum": checksum, "checksum": checksum,
               "yanked": self.yanked}
        body = row if "index.crates.io" in req.full_url else {"version": row}
        return contextlib.closing(io.BytesIO(json.dumps(body).encode()))


class RegistryTests(unittest.TestCase):
    def test_payload_preserves_normalized_metadata(self):
        extra = ('[dependencies.alias]\npackage = "original"\nversion = "1"\n'
                 'default-features = false\nfeatures = ["fast"]\noptional = true\n'
                 '[target.\'cfg(windows)\'.build-dependencies]\nwin = "2"\n')
        with tempfile.TemporaryDirectory() as td:
            path = Path(td) / "test.crate"
            crate(path, extra=extra)
            data = PREPARE.make_payload(path, "hyperdu", VERSION)
            size = struct.unpack_from("<I", data)[0]
            meta = json.loads(data[4:4 + size])
            self.assertEqual(meta["readme"], "概要")
            dep = meta["deps"][0]
            self.assertEqual((dep["name"], dep["explicit_name_in_toml"]),
                             ("original", "alias"))
            self.assertFalse(dep["default_features"])
            self.assertTrue(dep["optional"])
            self.assertEqual(meta["deps"][1]["target"], "cfg(windows)")
            self.assertEqual(meta["deps"][1]["kind"], "build")
            self.assertEqual(data[8 + size:], path.read_bytes())

    def test_rejects_unreviewed_dependency_sources(self):
        for source in ("path", "git", "registry", "registry-index"):
            with self.subTest(source=source), self.assertRaises(ValueError):
                list(PREPARE.dependency_rows({"dependencies": {
                    "x": {"version": "1", source: "untrusted"}}}))

    def test_rejects_wrong_identity_and_duplicate_manifest(self):
        with tempfile.TemporaryDirectory() as td:
            path = Path(td) / "test.crate"
            crate(path)
            with self.assertRaises(ValueError):
                PREPARE.make_payload(path, "hyperdu", "0.6.0")
            crate(path, duplicate=True)
            with self.assertRaises(ValueError):
                PREPARE.make_payload(path, "hyperdu", VERSION)

    def test_rejects_readme_outside_archive(self):
        with tempfile.TemporaryDirectory() as td:
            path = Path(td) / "test.crate"
            crate(path, readme=False, extra='readme = "../secret"\n')
            with self.assertRaises(ValueError):
                PREPARE.make_payload(path, "hyperdu", VERSION)

    def run_upload(self, registry, corrupt=None, environment=None):
        ns = uploader()
        with tempfile.TemporaryDirectory() as td, contextlib.chdir(td):
            Path("payloads").mkdir()
            for name in ns["CRATES"]:
                Path(f"payloads/{name}.bin").write_bytes(payload(name))
            if corrupt:
                corrupt()
            env = {"GITHUB_REF_NAME": "v" + VERSION, "REGISTRY_TOKEN": "test-only"}
            env.update(environment or {})
            with patch.dict(os.environ, env), patch.dict(ns, {"build_opener": lambda *a: registry}), \
                    patch("time.sleep"), contextlib.redirect_stdout(io.StringIO()):
                ns["main"]()

    def test_upload_order_and_token_boundary(self):
        registry = FakeRegistry()
        self.run_upload(registry)
        puts = [req for req in registry.calls if req.method == "PUT"]
        self.assertEqual(len(puts), 3)
        names = []
        for req in puts:
            size = struct.unpack_from("<I", req.data)[0]
            names.append(json.loads(req.data[4:4 + size])["name"])
            self.assertEqual(req.full_url, "https://crates.io/api/v1/crates/new")
            self.assertEqual(req.get_header("Authorization"), "test-only")
        self.assertEqual(names, list(PREPARE.CRATES))
        for req in registry.calls:
            if req.method == "GET":
                self.assertIsNone(req.get_header("Authorization"))

    def test_identical_existing_versions_are_not_reuploaded(self):
        registry = FakeRegistry(existing=True)
        self.run_upload(registry)
        self.assertFalse(any(req.method == "PUT" for req in registry.calls))

    def test_mismatched_or_yanked_versions_fail_closed(self):
        for kwargs in ({"wrong": True}, {"yanked": True}):
            registry = FakeRegistry(existing=True, **kwargs)
            with self.subTest(**kwargs), self.assertRaises(ValueError):
                self.run_upload(registry)
            self.assertFalse(any(req.method == "PUT" for req in registry.calls))

    def test_server_and_authorization_errors_are_not_missing_versions(self):
        for code in (401, 403, 429, 500):
            registry = FakeRegistry(error=code)
            with self.subTest(code=code), self.assertRaises(HTTPError):
                self.run_upload(registry)
            self.assertEqual(len(registry.calls), 1)

    def test_success_http_with_error_body_is_not_success(self):
        registry = FakeRegistry(reject=True)
        with self.assertRaises(ValueError):
            self.run_upload(registry)
        self.assertEqual(sum(req.method == "PUT" for req in registry.calls), 1)

    def test_validates_all_files_before_first_network_call(self):
        registry = FakeRegistry()
        with self.assertRaises(ValueError):
            self.run_upload(registry, lambda: Path("payloads/hyperdu-gui.bin").write_bytes(b"bad"))
        self.assertEqual(registry.calls, [])

    def test_missing_token_or_wrong_tag_stops_before_network(self):
        for env in ({"REGISTRY_TOKEN": ""}, {"GITHUB_REF_NAME": "main"}):
            registry = FakeRegistry()
            with self.subTest(env=env), self.assertRaises(ValueError):
                self.run_upload(registry, environment=env)
            self.assertEqual(registry.calls, [])

    def test_rejects_symlink_and_trailing_data(self):
        ns = uploader()
        with tempfile.TemporaryDirectory() as td:
            path = Path(td) / "payload"
            path.write_bytes(payload("hyperdu") + b"extra")
            with self.assertRaises(ValueError):
                ns["read_payload"](path, "hyperdu", VERSION)
            link = Path(td) / "link"
            link.symlink_to(path)
            with self.assertRaises(ValueError):
                ns["read_payload"](link, "hyperdu", VERSION)

    def test_redirects_are_rejected(self):
        with self.assertRaises(ValueError):
            uploader()["NoRedirect"]().redirect_request(None, None, 302, "", {}, "https://other.invalid")

    def test_missing_index_version_is_not_success(self):
        ns = uploader()
        with patch.dict(ns, {"request": lambda *a: b""}), patch("time.sleep"), self.assertRaises(ValueError):
            ns["indexed"](None, "hyperdu", VERSION, "0" * 64)

    def test_workflow_separates_builds_from_secrets(self):
        jobs = yaml.safe_load((ROOT / ".github/workflows/registry.yml").read_text())["jobs"]
        self.assertNotIn("secrets.", str(jobs["prepare"]))
        self.assertEqual(jobs["upload"]["needs"], "prepare")
        self.assertEqual(jobs["upload"]["permissions"], {"contents": "read"})
        self.assertNotIn("actions/checkout@", str(jobs["upload"]))
        self.assertNotRegex(str(jobs["upload"]), r"cargo |scripts/package/|scripts/lint/")
        self.assertEqual(jobs["upload"]["if"],
                         "github.event_name == 'push' && startsWith(github.ref, 'refs/tags/')")


if __name__ == "__main__":
    unittest.main()
