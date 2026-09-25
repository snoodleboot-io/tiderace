"""Benchmark corpus definitions — one place, used by every pass in this directory.

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
import os

R = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
VENDOR = os.path.join(R, "conformance", "vendor")
VENVS = os.environ.get("TIDERACE_BENCH_VENVS", os.path.join(R, ".tiderace-bench-venvs"))
PIRN = os.environ.get("PIRN_SNAPSHOT")


def _python(name: str) -> str:
    return os.environ.get(f"TIDERACE_PY_{name.upper().replace('-', '_')}") or os.path.join(
        VENVS, name, "bin", "python"
    )


def _public(name: str):
    root = os.path.join(VENDOR, name)
    return (name, "public", root, _python(name), "tests", os.path.join(root, "tests"), "")


def _internal(pkg: str):
    root = os.path.join(PIRN, "packages", pkg)
    return (pkg, "internal", root, os.path.join(PIRN, ".venv", "bin", "python"), "tests", root, "")


# (name, group, cwd, python, pytest target, tiderace root, extra PYTHONPATH for xdist)
CORPORA = [
    (
        "fx_corpus", "internal", os.path.join(R, "benchmarks", "fixtures", "fx_corpus"),
        os.environ.get("TIDERACE_PY_FX_CORPUS") or os.path.join(R, ".tiderace-fx-venv", "bin", "python"),
        "tests", os.path.join(R, "benchmarks", "fixtures", "fx_corpus", "tests"), "",
    ),
    *([_internal("pirn-data"), _internal("pirn-agents"), _internal("pirn-core")] if PIRN else []),
    _public("cachetools"),
    _public("click"),
    _public("flask"),
    _public("anyio"),
]

# `TIDERACE_BIN` lets a pass point at a binary built from a branch without touching the tree the
# other pass is using — the A/B between two builds has to be able to run them side by side.
TIDERACE = os.environ.get("TIDERACE_BIN", os.path.join(R, "engine", "target", "release", "tiderace"))
SHIM = os.path.join(R, "engine", "py-shim", "shim.py")
TR_PATH = os.path.join(R, "engine", "py-tiderace")


def by_name(name: str):
    return next(c for c in CORPORA if c[0] == name)


def load() -> float:
    """The one-minute load average — recorded with every measurement, never assumed."""
    return float(open("/proc/loadavg").read().split()[0])


def clean_env() -> dict:
    """The caller's environment minus anything that would leak one tool's settings into another."""
    return {k: v for k, v in os.environ.items()
            if k not in ("PYTHONPATH", "TIDERACE_SHIM", "TIDERACE_PYTHON", "TIDERACE_MARKER_EXPR",
                         "TIDERACE_KEYWORD_EXPR")}


def tiderace_env() -> dict:
    return dict(clean_env(), TIDERACE_SHIM=SHIM, PYTHONPATH=TR_PATH)
