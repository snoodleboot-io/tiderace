"""Snapshot/restore proof — fork-free isolation for IMPURE tests. Tests that mutate a shared global
(and would contaminate each other if run together) are run **in-process with no fork**, and the engine
restores the snapshot after each one — so they stay isolated without paying the fork. Real shim, no pytest.

Run:  python3 proof_snapshot_restore.py
"""
from __future__ import annotations

import os
import sys
import tempfile
import textwrap
import time

_HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, _HERE)
sys.path.insert(0, os.path.join(_HERE, os.pardir, "py-shim"))

import shim  # noqa: E402

N = 50
# Each test mutates a shared module global and asserts it STARTED clean — so without isolation they
# contaminate each other (n accumulates), but with per-test isolation each passes.
CORPUS = "_STATE = {'n': 0}\n" + "".join(
    f"def test_{i}():\n    _STATE['n'] += 1\n    assert _STATE['n'] == 1\n" for i in range(N)
)


def run_all(engine, nodes, force_no_fork):
    return sum(engine.run(n, "function", 5000, force_no_fork=force_no_fork)["outcome"] == "passed"
               for n in nodes)


def reset(module_name="test_restore"):
    sys.modules[module_name]._STATE["n"] = 0


def main() -> int:
    print("=== snapshot/restore proof: fork-free isolation for impure tests (real shim) ===\n")
    with tempfile.TemporaryDirectory() as root:
        with open(os.path.join(root, "test_restore.py"), "w") as f:
            f.write(CORPUS)
        sys.path.insert(0, root)
        shim._preimport(root)
        reg = shim._discover(root)
        nodes = [f"test_restore.py::test_{i}" for i in range(N)]

        # 1. No fork, NO restore → contamination (n accumulates ⇒ most fail).
        e = shim.Engine(reg, no_fork=False, root=root)
        reset()
        no_restore_pass = run_all(e, nodes, force_no_fork=True)

        # 2. No fork, WITH restore → isolated (each starts clean ⇒ all pass), and time it.
        e_r = shim.Engine(reg, no_fork=False, root=root, restore=True)
        reset()
        t0 = time.perf_counter()
        restore_pass = run_all(e_r, nodes, force_no_fork=True)
        restore_ms = (time.perf_counter() - t0) * 1000

        # 3. Fork (the usual isolation) → all pass, baseline time.
        reset()
        t0 = time.perf_counter()
        fork_pass = run_all(e, nodes, force_no_fork=False)
        fork_ms = (time.perf_counter() - t0) * 1000
        e.teardown_all(); e_r.teardown_all()

        print(f"    no-fork, NO restore : {no_restore_pass:>3}/{N} passed   (contamination — not isolated)")
        print(f"    no-fork, RESTORE    : {restore_pass:>3}/{N} passed   in {restore_ms:6.1f} ms  (isolated, no fork)")
        print(f"    fork per test       : {fork_pass:>3}/{N} passed   in {fork_ms:6.1f} ms  (isolated, with fork)")
        print(f"    restore vs fork     : {fork_ms / max(restore_ms, 0.001):.0f}× faster, same isolation")

        go = (
            no_restore_pass < N        # without restore: genuine contamination
            and restore_pass == N      # with restore: fully isolated
            and fork_pass == N         # fork: the isolation baseline
            and restore_ms < fork_ms   # and restore is faster than fork
        )
        print(f"\n=== VERDICT: {'GO — impure tests isolated WITHOUT a fork via snapshot/restore' if go else 'NO-GO'} ===")
        return 0 if go else 1


if __name__ == "__main__":
    raise SystemExit(main())
