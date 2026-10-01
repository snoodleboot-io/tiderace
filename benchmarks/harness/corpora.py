"""Benchmark corpus definitions — one place, used by every pass in this package.

Nothing here is an absolute path. Public corpora are the vendored checkouts under
`conformance/vendor/`; each needs a virtualenv with that project's pinned pytest, found at
`$TIDERACE_BENCH_VENVS/<name>/bin/python` (default `.tiderace-bench-venvs/` at the repo root) or
overridden per corpus with `TIDERACE_PY_<NAME>`. The internal corpora are a **snapshot** of the
monorepo — source at one commit plus a copy of its virtualenv — named by `PIRN_SNAPSHOT`; without
it they are simply absent from the list.

Why a snapshot rather than the live checkout: the live checkout's venv was being installed into
*during* an earlier pass, which moved pytest's own totals between runs. A benchmark needs one fixed
environment, and a corpus that moves is itself worth reporting, so keep the snapshots.
"""
from __future__ import annotations

import os
from dataclasses import dataclass

R = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
VENDOR = os.path.join(R, "conformance", "vendor")
VENVS = os.environ.get("TIDERACE_BENCH_VENVS", os.path.join(R, ".tiderace-bench-venvs"))
PIRN = os.environ.get("PIRN_SNAPSHOT")


@dataclass(frozen=True)
class Corpus:
    """One suite the harness runs: where it lives, which interpreter runs it, and what each
    runner is pointed at. A pass used to unpack this as a seven-tuple, by position."""

    name: str
    group: str  # "public" (a vendored checkout) or "internal" (the monorepo snapshot)
    cwd: str  # the directory both runners are invoked from
    python: str  # the corpus's own interpreter — its pinned pytest
    pytest_target: str  # what pytest is pointed at, relative to `cwd`
    tiderace_root: str  # the run root tiderace is pointed at
    xdist_path: str = ""  # extra PYTHONPATH supplying pytest-xdist where the venv has none

    @property
    def prefix(self) -> str:
        """tiderace's node ids are relative to the run root and pytest's to `cwd`: the path that
        maps one onto the other, `'.'` when they coincide."""
        return os.path.relpath(self.tiderace_root, self.cwd)


def _venv_python(venv: str) -> str:
    """The interpreter of a venv, where this platform keeps it."""
    if os.name == "nt":
        return os.path.join(venv, "Scripts", "python.exe")
    return os.path.join(venv, "bin", "python")


def _python(name: str) -> str:
    return os.environ.get(f"TIDERACE_PY_{name.upper().replace('-', '_')}") or _venv_python(
        os.path.join(VENVS, name)
    )


def _public(name: str) -> Corpus:
    root = os.path.join(VENDOR, name)
    return Corpus(name, "public", root, _python(name), "tests", os.path.join(root, "tests"))


# pytest-xdist for the internal corpora, installed to a directory of its own rather than into the
# snapshot's venv — the snapshot is a faithful copy of the project's environment, and xdist is the
# benchmark's comparison, not the project's dependency. `uv pip install --target` it here.
XDIST_PATH = os.environ.get("TIDERACE_XDIST_PATH", os.path.join(R, ".tiderace-bench-venvs", "xdist"))


def _internal(pkg: str) -> Corpus:
    root = os.path.join(PIRN, "packages", pkg)
    return Corpus(pkg, "internal", root, os.path.join(PIRN, ".venv", "bin", "python"), "tests",
                  root, XDIST_PATH)


_FX = os.path.join(R, "benchmarks", "fixtures", "fx_corpus")
CORPORA: tuple[Corpus, ...] = (
    Corpus(
        "fx_corpus", "internal", _FX,
        os.environ.get("TIDERACE_PY_FX_CORPUS") or _venv_python(os.path.join(R, ".tiderace-fx-venv")),
        "tests", os.path.join(_FX, "tests"),
    ),
) + (
    (_internal("pirn-core"), _internal("pirn-agents"), _internal("pirn-data")) if PIRN else ()
) + (
    _public("cachetools"),
    _public("click"),
    _public("flask"),
    _public("anyio"),
)

# `TIDERACE_BIN` lets a pass point at a binary built from a branch without touching the tree the
# other pass is using — the A/B between two builds has to be able to run them side by side.
TIDERACE = os.environ.get("TIDERACE_BIN", os.path.join(
    R, "engine", "target", "release", "tiderace.exe" if os.name == "nt" else "tiderace"))
# `TIDERACE_SHIM` / `TIDERACE_PY_TIDERACE` point a pass at a branch's shim and package the same way
# `TIDERACE_BIN` points it at a branch's binary (read here, before `clean_env` strips them).
SHIM = os.environ.get("TIDERACE_SHIM") or os.path.join(R, "engine", "py-shim", "shim.py")
TR_PATH = os.environ.get("TIDERACE_PY_TIDERACE") or os.path.join(R, "engine", "py-tiderace")


def by_name(name: str) -> Corpus:
    return next(c for c in CORPORA if c.name == name)


def load_average() -> float:
    """The one-minute load average — recorded with every measurement, never assumed. Zero where
    the platform has none to report (Windows)."""
    try:
        return os.getloadavg()[0]
    except (AttributeError, OSError):
        return 0.0


def clean_env() -> dict:
    """The caller's environment minus anything that would leak one tool's settings into another."""
    return {k: v for k, v in os.environ.items()
            if k not in ("PYTHONPATH", "TIDERACE_SHIM", "TIDERACE_PYTHON", "TIDERACE_MARKER_EXPR",
                         "TIDERACE_KEYWORD_EXPR")}


def tiderace_env() -> dict:
    return dict(clean_env(), TIDERACE_SHIM=SHIM, PYTHONPATH=TR_PATH)
