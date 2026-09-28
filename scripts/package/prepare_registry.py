"""Create data-only registry requests from Cargo-verified .crate archives.

Run in an unprivileged job, AFTER `cargo package --workspace --locked`.
Protocol: https://doc.rust-lang.org/cargo/reference/registry-web-api.html#publish
This module never publishes or reads credentials.
"""

import argparse
import json
import re
import struct
import tarfile
import tomllib
from pathlib import Path

CRATES = ("hyperdu-core", "hyperdu", "hyperdu-gui")
MAX_BYTES = 32 * 1024 * 1024
VERSION = re.compile(r"[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?")


def read_member(archive, name):
    """Read a bounded regular file without extracting archive paths."""
    matches = [item for item in archive.getmembers() if item.name == name]
    if len(matches) != 1 or not matches[0].isfile():
        raise ValueError(f"expected one regular archive member: {name}")
    member = matches[0]
    if member.size > MAX_BYTES:
        raise ValueError(f"archive member too large: {name}")
    with archive.extractfile(member) as stream:
        return stream.read(MAX_BYTES + 1).decode("utf-8")


def dependency_rows(table, target=None):
    for section, kind in (("dependencies", "normal"), ("dev-dependencies", "dev"),
                          ("build-dependencies", "build")):
        for alias, value in table.get(section, {}).items():
            spec = {"version": value} if isinstance(value, str) else value
            # Cargo's normalized publish manifest must contain registry deps,
            # not paths/git sources or an unreviewed alternate registry.
            if any(key in spec for key in ("path", "git", "registry", "registry-index")):
                raise ValueError(f"unsupported dependency source: {alias}")
            if not isinstance(spec.get("version"), str):
                raise ValueError(f"missing dependency version: {alias}")
            original = spec.get("package", alias)
            yield {
                "name": original, "version_req": spec["version"],
                "features": spec.get("features", []),
                "optional": spec.get("optional", False),
                "default_features": spec.get("default-features", True),
                "target": target, "kind": kind, "registry": None,
                "explicit_name_in_toml": alias if alias != original else None,
            }


def make_payload(path, name, version):
    if name not in CRATES or not VERSION.fullmatch(version):
        raise ValueError("unexpected crate name or version")
    if path.stat().st_size > MAX_BYTES:
        raise ValueError("crate exceeds size limit")
    crate = path.read_bytes()
    prefix = f"{name}-{version}/"
    with tarfile.open(path, "r:gz") as archive:
        manifest = tomllib.loads(read_member(archive, prefix + "Cargo.toml"))
        package = manifest["package"]
        if (package["name"], package["version"]) != (name, version):
            raise ValueError("archive package identity does not match release")
        deps = list(dependency_rows(manifest))
        for target, table in manifest.get("target", {}).items():
            deps.extend(dependency_rows(table, target))
        readme_path = package.get("readme")
        readme = None
        if isinstance(readme_path, str):
            if Path(readme_path).is_absolute() or ".." in Path(readme_path).parts:
                raise ValueError("README must be inside the packaged crate")
            readme = read_member(archive, prefix + readme_path)
        else:
            readme_path = None
        metadata = {
            "name": name, "vers": version, "deps": deps,
            "features": manifest.get("features", {}),
            "authors": package.get("authors", []),
            "description": package.get("description"),
            "documentation": package.get("documentation"),
            "homepage": package.get("homepage"),
            "readme": readme, "readme_file": readme_path,
            "keywords": package.get("keywords", []),
            "categories": package.get("categories", []),
            "license": package.get("license"),
            "license_file": package.get("license-file"),
            "repository": package.get("repository"),
            "links": package.get("links"),
            "rust_version": package.get("rust-version"),
            "badges": manifest.get("badges", {}),
        }
    data = json.dumps(metadata, ensure_ascii=False, separators=(",", ":")).encode()
    payload = struct.pack("<I", len(data)) + data + struct.pack("<I", len(crate)) + crate
    if len(payload) > MAX_BYTES:
        raise ValueError("registry request exceeds size limit")
    return payload


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--version", required=True)
    parser.add_argument("--package-dir", type=Path, default=Path("target/package"))
    parser.add_argument("--output", type=Path, default=Path("registry-payloads"))
    args = parser.parse_args()
    if not VERSION.fullmatch(args.version):
        parser.error("invalid release version")
    # Validate all packages before writing any upload payload.
    payloads = {name: make_payload(args.package_dir / f"{name}-{args.version}.crate",
                                   name, args.version) for name in CRATES}
    args.output.mkdir(parents=True, exist_ok=False)
    for name, payload in payloads.items():
        (args.output / f"{name}.bin").write_bytes(payload)
    print(f"Prepared {len(payloads)} data-only registry requests for {args.version}")


if __name__ == "__main__":
    main()
