"""Proof: opaque modules on a fork-less platform (Windows) — crash and silent-leak, both fixed.

The no-fork ladder's soundness argument is "an opaque module still forks." Windows has no `fork()`,
so that bottom rung doesn't exist there — and the shim handled it in two broken ways:

1. **Optimistic no-fork path** (`force_no_fork`, what the daemon pool drives). A module that isn't
   snapshot-restorable demotes to fork → `os.fork()` → `AttributeError: module 'os' has no attribute
   'fork'`, *uncaught*, propagating out of `Engine.run` and killing the worker.

2. **`--no-fork` mode** (`Engine(no_fork=True)` — the SubprocessWorker / Windows path from TID-5).
   The restorability check was gated on `force_no_fork`, so whole-run no-fork skipped it entirely and
   ran opaque modules **in-process anyway**. Un-restorable state then leaked between tests: a
   module-level generator stayed advanced. Silently wrong results, and platform-independent — on
   Linux `--no-fork` had the same leak.

Fix: the restorability gate now covers both paths and yields `must_fork`, which overrides both
in-process branches; when fork is unavailable the test is reported as an **error** rather than run
without isolation. A wrong green is worse than a reported error.

Windows is simulated by deleting `os.fork`, which is how CPython presents that platform.

Run:  .tiderace-fx-venv/bin/python engine/py-tiderace/proof_windows_opaque_fork.py
"""

from __future__ import annotations

import importlib
import os
import pathlib
import sys
import tempfile

ROOT = pathlib.Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "py-shim"))
import shim  # noqa: E402

# An *opaque* module: a generator global that deepcopy can't reproduce, so `_restorable()` is False.
# `test_a` advances it; `test_b` detects whether that advance leaked.
OPAQUE = """
_GEN = (i for i in range(100))

def test_a():
    assert next(_GEN) == 0

def test_b():
    v = next(_GEN)
    assert v == 0, f"LEAK: generator advanced across tests, got {v}"
"""

PURE = """
def test_pure():
    assert 1 + 1 == 2
"""


def corpus() -> str:
    tmp = tempfile.mkdtemp(prefix="tiderace_winfork_")
    (pathlib.Path(tmp) / "test_opaque.py").write_text(OPAQUE)
    (pathlib.Path(tmp) / "test_pure.py").write_text(PURE)
    if tmp not in sys.path:
        sys.path.insert(0, tmp)
    return tmp


def run(root: str, node: str, *, no_fork: bool, force: bool) -> tuple[str, str]:
    """-> (outcome, detail). Fresh Engine per call; `Engine.run` returns the wire dict."""
    for m in ("test_opaque", "test_pure"):
        sys.modules.pop(m, None)
    importlib.invalidate_caches()
    eng = shim.Engine(shim._discover(root), root=root, no_fork=no_fork, restore=True)
    r = eng.run(node, "Function", 5000, force_no_fork=force)
    return r.get("outcome", "?"), r.get("detail", "")


OPAQUE_A, OPAQUE_B = "test_opaque.py::test_a", "test_opaque.py::test_b"
PURE_N = "test_pure.py::test_pure"


def main() -> int:
    root = corpus()
    failures: list[str] = []

    def check(label: str, got, want) -> None:
        ok = got == want
        print(f"  {'ok  ' if ok else 'FAIL'}  {label}: {got!r}")
        if not ok:
            failures.append(f"{label}: expected {want!r}, got {got!r}")

    # ---- 1. WITH fork (Linux baseline): the ladder forks opaque modules; nothing leaks ----
    print(f"os.fork present: {hasattr(os, 'fork')}")
    print("\n[1] with fork — opaque module forks, no leak")
    check("opaque test_a", run(root, OPAQUE_A, no_fork=False, force=True)[0], "passed")
    check("opaque test_b", run(root, OPAQUE_B, no_fork=False, force=True)[0], "passed")

    saved = getattr(os, "fork", None)
    if saved is None:
        print("\nno os.fork on this platform — running the fork-less half natively")
    else:
        del os.fork
    # `_FORK_AVAILABLE` is computed at import; re-evaluate it for the simulation.
    shim._FORK_AVAILABLE = hasattr(os, "fork")
    try:
        # ---- 2. WITHOUT fork, optimistic path: must not raise; must refuse ----
        print("\n[2] without fork, optimistic no-fork — refuses instead of crashing")
        try:
            oc, detail = run(root, OPAQUE_A, no_fork=False, force=True)
        except AttributeError as exc:                       # the pre-fix behaviour
            print(f"  FAIL  uncaught {type(exc).__name__}: {exc}")
            failures.append("optimistic path raised instead of reporting an error")
            oc, detail = "<raised>", ""
        check("opaque outcome", oc, "error")
        if oc == "error":
            print(f"        reason: {detail.strip()[:100]}…")

        # ---- 3. WITHOUT fork, --no-fork mode: must refuse, NOT leak ----
        print("\n[3] without fork, --no-fork mode — refuses instead of leaking")
        check("opaque test_a", run(root, OPAQUE_A, no_fork=True, force=False)[0], "error")
        check("opaque test_b", run(root, OPAQUE_B, no_fork=True, force=False)[0], "error")

        # ---- 4. the fix must not cost the common case: pure modules still run ----
        print("\n[4] without fork — restorable/pure modules are unaffected")
        check("pure, --no-fork", run(root, PURE_N, no_fork=True, force=False)[0], "passed")
        check("pure, optimistic", run(root, PURE_N, no_fork=False, force=True)[0], "passed")
    finally:
        if saved is not None:
            os.fork = saved
        shim._FORK_AVAILABLE = hasattr(os, "fork")

    print()
    if failures:
        print("PROOF FAILED:")
        for f in failures:
            print(f"  - {f}")
        return 1
    print("PROOF OK — opaque modules on a fork-less platform are refused, not crashed and not leaked;\n"
          "           pure/restorable modules keep running in-process.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
