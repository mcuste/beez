#!/usr/bin/env python3
"""Check the workspace version, the changelog, and an optional release tag."""

from __future__ import annotations

import re
import sys
import tomllib
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
CARGO_MANIFEST = ROOT / "Cargo.toml"
CHANGELOG = ROOT / "CHANGELOG.md"
VERSION_PATTERN = re.compile(r"^\d+\.\d+\.\d+$")


def fail(message: str) -> None:
    raise SystemExit(f"check-version: {message}")


def workspace_version() -> str:
    with CARGO_MANIFEST.open("rb") as source:
        return tomllib.load(source)["workspace"]["package"]["version"]


def main() -> None:
    if len(sys.argv) > 2:
        fail("usage: check-version.py [v<version>]")

    version = workspace_version()
    if VERSION_PATTERN.fullmatch(version) is None:
        fail(f"Cargo.toml version {version!r} is not a three-part version")

    changelog = CHANGELOG.read_text()
    if "## [Unreleased]" not in changelog:
        fail("CHANGELOG.md has no [Unreleased] section")

    if len(sys.argv) == 2:
        tag = sys.argv[1]
        if tag != f"v{version}":
            fail(f"release tag must be v{version}, got {tag!r}")
        if re.search(rf"^## \[{re.escape(version)}\]", changelog, flags=re.MULTILINE) is None:
            fail(f"CHANGELOG.md has no section for {version}")


if __name__ == "__main__":
    main()
