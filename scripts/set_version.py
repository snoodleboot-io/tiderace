#!/usr/bin/env python3
"""
Pin a derived version into the dist crate's pyproject.toml.

    python scripts/set_version.py 0.3.0

This exists because the wheel is now built on five platforms and the previous
`sed -i "s/^version = .*/.../"` is not portable: BSD sed (macOS) requires an argument to
-i, so the GNU form silently consumes the next token as the backup suffix and mangles the
invocation. Doing the rewrite in Python keeps one code path across linux/macos/windows.

Only the `version` key inside the `[project]` table is touched — `[build-system]` and any
future table keep their own keys. Fails loudly if the key is not found, because a silent
no-op here would publish a wheel carrying the placeholder version.
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

PYPROJECT = Path(__file__).resolve().parent.parent / "engine" / "crates" / "tiderace-dist" / "pyproject.toml"

# A TOML table header: `[project]`, `[tool.maturin]`, ... Used to bound the [project] table.
TABLE_HEADER = re.compile(r"^\s*\[")
VERSION_KEY = re.compile(r"^(\s*version\s*=\s*)(.+)$")


def pin_version(text: str, version: str) -> str:
    """Return `text` with the [project] table's version replaced by `version`."""
    lines = text.splitlines(keepends=True)
    in_project = False
    for i, line in enumerate(lines):
        if TABLE_HEADER.match(line):
            in_project = line.strip() == "[project]"
            continue
        if in_project:
            match = VERSION_KEY.match(line)
            if match:
                lines[i] = f'{match.group(1)}"{version}"\n'
                return "".join(lines)
    raise SystemExit(f"error: no `version` key found in the [project] table of {PYPROJECT}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("version", help="the derived version, e.g. 0.3.0")
    args = parser.parse_args()

    original = PYPROJECT.read_text(encoding="utf-8")
    PYPROJECT.write_text(pin_version(original, args.version), encoding="utf-8")
    print(f"pinned version = {args.version} in {PYPROJECT}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
