"""Offline documentation contracts; optionally verify the rendered Jekyll site.

Usage: python3 scripts/lint/test_docs.py [--rendered _site]
Requires PyYAML. Does not query release servers or infer publication from CI.
The published version inventory must be updated after publication is verified.
"""

import argparse
import html
import json
import re
import shlex
import tomllib
import unittest
from pathlib import Path
from urllib.parse import unquote, urlsplit

import yaml

ROOT = Path(__file__).resolve().parents[2]
SUFFIXES = ("", ".en", ".zh-CN")
# The repository front page advertises the published release.
PUBLIC_READMES = [ROOT / ("README" + suffix + ".md") for suffix in SUFFIXES]
# Crate READMEs are packaged into the crate and become its permanent crates.io
# page, so they name the version they ship with: the workspace version.
CRATE_READMES = [ROOT / directory / ("README" + suffix + ".md")
                 for directory in ("hyperdu", "hyperdu-gui", "hyperdu-core")
                 for suffix in SUFFIXES]
READMES = PUBLIC_READMES + [path for path in CRATE_READMES
                            if path.parent.name != "hyperdu-core"]
DOCUMENTS = READMES + [ROOT / "docs/developer-guide.md"] + [
    ROOT / "docs" / directory / "setup.md" for directory in (".", "en", "zh-CN")]
RENDERED = None
STALE = ("公開準備中", "being prepared for publication", "正在准备发布",
         "acceleration build featured here", "高速化開発版は、下のコマンド")


def markdown_targets(text):
    """Inline destinations used by these documents; ignore fenced code blocks."""
    without_code = re.sub(r"```[^\n]*\n.*?```", "", text, flags=re.S)
    return re.findall(r"\]\(([^\s)]+)(?:\s+\"[^\"]*\")?\)", without_code)


def heading_ids(text):
    ids = set(re.findall(r'\bid=[\"\']([^\"\']+)[\"\']', text))
    counts = {}
    for heading in re.findall(r"^#{1,6}\s+(.+?)\s*#*\s*$", text, flags=re.M):
        slug = re.sub(r"[^\w\- ]", "", heading.lower()).replace(" ", "-")
        count = counts.get(slug, 0)
        counts[slug] = count + 1
        ids.add(slug if count == 0 else f"{slug}-{count}")
    return ids


class DocumentationTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.config = yaml.safe_load((ROOT / "site/_config.yml").read_text())
        cls.copy = json.loads((ROOT / "site/_data/copy.json").read_text())
        cls.landing = (ROOT / "site/_includes/landing.html").read_text()
        cls.bucket = json.loads((ROOT / "bucket/hyperdu.json").read_text())
        manifest = tomllib.loads((ROOT / "Cargo.toml").read_text())
        cls.workspace_version = manifest["workspace"]["package"]["version"]

    def install_versions(self, path):
        commands = [line for line in path.read_text().splitlines()
                    if line.startswith("cargo install ") and "--version" in line]
        self.assertTrue(commands, f"missing install command: {path}")
        for command in commands:
            words = shlex.split(command)
            self.assertIn("--locked", words, str(path))
            yield words[words.index("--version") + 1]

    def test_installation_commands_use_published_versions(self):
        version = self.bucket["version"]
        self.assertEqual(self.config["versions"], {"cli": version, "gui": version})
        asset = self.bucket["architecture"]["64bit"]["url"]
        self.assertIn(f"/releases/download/v{version}/", asset)
        for path in PUBLIC_READMES:
            for found in self.install_versions(path):
                self.assertEqual(found, version, str(path))

    def test_crate_readmes_name_the_version_they_ship_with(self):
        for path in READMES:
            if path in CRATE_READMES:
                for found in self.install_versions(path):
                    self.assertEqual(found, self.workspace_version, str(path))
        for path in CRATE_READMES:
            text = path.read_text()
            self.assertIn(f"`{self.workspace_version}`", text, str(path))
            for claim in ("公開済み", "is published", "已发布"):
                self.assertNotIn(f"{self.workspace_version}` {claim}", text, str(path))

    def test_no_stale_publication_notice(self):
        for path in READMES + [ROOT / "site/_data/copy.json"]:
            for phrase in STALE:
                self.assertNotIn(phrase, path.read_text(), str(path))

    def test_local_markdown_targets_and_anchors_exist(self):
        errors = []
        for path in DOCUMENTS:
            for target in markdown_targets(path.read_text()):
                url = urlsplit(target)
                if url.scheme or url.netloc:
                    continue
                destination = (path.parent / unquote(url.path)).resolve() if url.path else path
                if not destination.exists():
                    errors.append(f"{path.relative_to(ROOT)} -> {target}: missing path")
                elif url.fragment and destination.suffix == ".md":
                    if unquote(url.fragment) not in heading_ids(destination.read_text()):
                        errors.append(f"{path.relative_to(ROOT)} -> {target}: missing heading")
        self.assertEqual(errors, [], "\n".join(errors))

    def test_all_site_languages_have_the_same_contract(self):
        languages = {entry["code"] for entry in self.config["languages"]}
        self.assertEqual(set(self.copy), languages)
        keys = set(self.copy["ja"])
        referenced = set(re.findall(r"\bt\.(\w+)", self.landing))
        for language, content in self.copy.items():
            self.assertEqual(set(content), keys, language)
            self.assertFalse(referenced - set(content), language)
            self.assertNotRegex(content["status"], r"\d+\.\d+\.\d+")
            self.assertEqual([tool["name"] for tool in content["tools"]],
                             ["list_volumes", "scan_path", "find_reclaimable"])
            for link in content["doc_links"]:
                self.assertTrue((ROOT / link["path"]).is_file(), link["path"])

    def test_source_measurement_and_release_are_separate(self):
        self.assertEqual(self.config["source_revision"], "main")
        bench = json.loads((ROOT / "site/_data/benchmarks.json").read_text())
        self.assertRegex(bench["commit"], r"^[0-9a-f]{40}$")
        self.assertIn("bench.commit", self.landing)
        self.assertIn("cargo install hyperdu --locked --version {{ site.versions.cli }}",
                      self.landing)
        self.assertIn("v{{ site.versions.cli }} · {{ t.status }}", self.landing)
        self.assertNotIn("cargo install --locked --git", self.landing)
        self.assertNotIn("macOS <small>CLI · native CI", self.landing)
        self.assertIn("{{ t.mac_status }}", self.landing)

    def test_version_fixer_cannot_advance_public_installation_docs(self):
        source = (ROOT / "scripts/lint/versions.sh").read_text()
        def inventory(name):
            match = re.search(rf"^{name}=\((.*?)\)", source, flags=re.M | re.S)
            self.assertIsNotNone(match)
            return set(shlex.split(match[1], comments=True))
        workspace, released = inventory("WORKSPACE_FILES"), inventory("RELEASE_FILES")
        self.assertFalse(workspace & released)
        for path in PUBLIC_READMES + [ROOT / "site/_config.yml"]:
            self.assertIn(path.relative_to(ROOT).as_posix(), released)
        for path in CRATE_READMES:
            self.assertIn(path.relative_to(ROOT).as_posix(), workspace)
        self.assertIn("plugin/plugin.json", workspace)
        self.assertIn("snap/snapcraft.yaml", workspace)

    def test_link_parser_ignores_examples_and_external_urls(self):
        sample = "[local](../README.md#gui)\n```rust\n[not](missing)\n```\n[web](https://example.invalid/)"
        self.assertEqual(markdown_targets(sample), ["../README.md#gui", "https://example.invalid/"])
        self.assertEqual(heading_ids('# GUI\n## GUI\n<a id="manual"></a>'),
                         {"gui", "gui-1", "manual"})

    def test_rendered_site(self):
        if RENDERED is None:
            self.skipTest("pass --rendered after the Jekyll build")
        command = f"cargo install hyperdu --locked --version {self.bucket['version']}"
        for entry in self.config["languages"]:
            path = RENDERED / entry["path"].strip("/") / "index.html"
            self.assertTrue(path.is_file(), str(path))
            page = html.unescape(path.read_text())
            copy = self.copy[entry["code"]]
            self.assertIn(f'<code id="install-code">{command}</code>', page, str(path))
            self.assertIn(f"v{self.bucket['version']} · {copy['status']}", page, str(path))
            self.assertIn(copy["mac_status"], page, str(path))
            self.assertNotIn("cargo install --locked --git", page, str(path))
            self.assertNotIn("{{ t.", page, str(path))
            for phrase in STALE:
                self.assertNotIn(phrase, page, str(path))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rendered", type=Path)
    args = parser.parse_args()
    RENDERED = args.rendered
    unittest.main(argv=[__file__])
