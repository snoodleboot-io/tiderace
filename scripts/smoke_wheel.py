#!/usr/bin/env python3
"""
Install a built wheel into a throwaway venv and prove it runs a corpus with NO env vars.

    python scripts/smoke_wheel.py --dist dist

The guarantee under test: a fresh `pip install tiderace` puts `tiderace` and
`tiderace-daemon` on PATH, and the daemon auto-locates the bundled shim
(`tiderace/_shim/shim.py`) without TIDERACE_SHIM or TIDERACE_PYTHON being set. Those two
variables are explicitly scrubbed below — with them set, this test would pass against a
wheel that ships no shim at all.

This replaces the inline bash that lived in ci.yml. That version hardcoded POSIX layout
(`/tmp/smoke/bin/python`) and an extensionless binary name, so it could not run on the
Windows or macOS legs the wheel matrix added. `venv_bin()` and `shutil.which()` handle the
Scripts/ vs bin/ split and the `.exe` suffix.

`run --all` exits non-zero pytest-style when a test fails, and the corpus deliberately
contains one failing test — so the exit code is expected to be non-zero and the assertion
is on the reported output, not on the status.

The venv is created with `uv`, matching what ci.yml already did. `python -m venv` is not a
safe substitute: Debian/Ubuntu split `ensurepip` into a separate `python3-venv` package, so
stdlib venv creation fails outright on a stock interpreter. uv carries its own bootstrap and
is materially faster, which is worth having now that this runs on five legs instead of one.
"""

from __future__ import annotations

import argparse
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

CORPUS = "def test_pass():\n    assert 1 + 1 == 2\n\n\ndef test_fail():\n    assert 1 == 2\n"
EXPECTED = "1 failing"


def venv_bin(venv: Path) -> Path:
    """Console-script directory for a venv — Windows uses Scripts\\, POSIX uses bin/."""
    return venv / ("Scripts" if os.name == "nt" else "bin")


def find_wheel(dist: Path) -> Path:
    wheels = sorted(dist.glob("*.whl"))
    if not wheels:
        raise SystemExit(f"error: no wheel found in {dist}")
    if len(wheels) > 1:
        raise SystemExit(f"error: expected exactly one wheel in {dist}, found {[w.name for w in wheels]}")
    return wheels[0]


def run(cmd: list[str], **kwargs: object) -> subprocess.CompletedProcess[str]:
    print(f"$ {' '.join(str(c) for c in cmd)}", flush=True)
    return subprocess.run([str(c) for c in cmd], text=True, **kwargs)  # type: ignore[arg-type]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dist", default="dist", type=Path, help="directory holding the built wheel")
    args = parser.parse_args()

    wheel = find_wheel(args.dist.resolve())
    print(f"smoke-testing {wheel.name}")

    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        venv = root / "venv"
        corpus = root / "corpus"
        corpus.mkdir()
        (corpus / "test_smoke.py").write_text(CORPUS, encoding="utf-8")

        uv = shutil.which("uv")
        if uv is None:
            raise SystemExit("error: uv not found on PATH (CI installs it alongside maturin)")

        run([uv, "venv", str(venv), "--python", sys.executable], check=True)
        python = venv_bin(venv) / ("python.exe" if os.name == "nt" else "python")
        run([uv, "pip", "install", "--python", str(python), str(wheel), "numpy", "pytest"], check=True)

        # The authoring package and the staged shim must both be importable from the wheel.
        run([python, "-c", "import tiderace, tiderace._shim"], check=True)

        # Zero-config proof: venv bin on PATH, and the two override vars removed entirely.
        env = dict(os.environ)
        env["PATH"] = str(venv_bin(venv)) + os.pathsep + env.get("PATH", "")
        env.pop("TIDERACE_SHIM", None)
        env.pop("TIDERACE_PYTHON", None)

        daemon = shutil.which("tiderace-daemon", path=str(venv_bin(venv)))
        if daemon is None:
            raise SystemExit(f"error: tiderace-daemon not installed into {venv_bin(venv)}")

        result = run(
            [daemon, "run", ".", "--all"],
            cwd=str(corpus),
            env=env,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
        )
        print(result.stdout)
        if EXPECTED not in (result.stdout or ""):
            raise SystemExit(f"error: smoke test did not run the corpus as expected (no {EXPECTED!r} in output)")

    print("smoke test OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
