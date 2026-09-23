#!/usr/bin/env python3
"""Print the Homebrew formula for a release tag, using the checksums in SHA256SUMS."""

from __future__ import annotations

import re
import sys
from pathlib import Path
from typing import NoReturn


TAG_PATTERN = re.compile(r"^v(\d+\.\d+\.\d+)$")
REPOSITORY = "https://github.com/mcuste/beez"
DESCRIPTION = "Run coding agents and commands in sandboxed workflows"
TARGETS = {
    ("macos", "arm"): "aarch64-apple-darwin",
    ("macos", "intel"): "x86_64-apple-darwin",
    ("linux", "arm"): "aarch64-unknown-linux-gnu",
    ("linux", "intel"): "x86_64-unknown-linux-gnu",
}


def fail(message: str) -> NoReturn:
    raise SystemExit(f"homebrew-formula: {message}")


def read_checksums(path: Path) -> dict[str, str]:
    checksums = {}
    for line in path.read_text().splitlines():
        digest, name = line.split(maxsplit=1)
        checksums[name.lstrip("*")] = digest
    return checksums


def platform_block(system: str, tag: str, checksums: dict[str, str]) -> str:
    lines = [f"  on_{system} do"]
    for cpu in ("arm", "intel"):
        asset = f"beez-{tag}-{TARGETS[(system, cpu)]}.tar.gz"
        if asset not in checksums:
            fail(f"SHA256SUMS has no entry for {asset}")
        lines += [
            f"    on_{cpu} do",
            f'      url "{REPOSITORY}/releases/download/{tag}/{asset}"',
            f'      sha256 "{checksums[asset]}"',
            "    end",
        ]
    lines.append("  end")
    return "\n".join(lines)


def main() -> None:
    if len(sys.argv) != 3:
        fail("usage: homebrew-formula.py v<version> <SHA256SUMS>")

    tag = sys.argv[1]
    if TAG_PATTERN.fullmatch(tag) is None:
        fail(f"invalid release tag {tag!r}")

    checksums = read_checksums(Path(sys.argv[2]))
    print(f'''class Beez < Formula
  desc "{DESCRIPTION}"
  homepage "{REPOSITORY}"
  license "MIT"

{platform_block("macos", tag, checksums)}

{platform_block("linux", tag, checksums)}

  def install
    bin.install "beez"
  end

  test do
    assert_match version.to_s, shell_output("#{{bin}}/beez --version")
  end
end''')


if __name__ == "__main__":
    main()
