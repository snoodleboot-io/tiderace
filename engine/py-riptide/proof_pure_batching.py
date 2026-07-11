"""Pure-test batching proof — the payoff of the purity guard. Pure tests run in-process (no fork) at
~90× the speed of fork-per-test, with identical outcomes; the purity guard re-checks each one so a
misclassified mutator is flagged (defense in depth). Drives the real shim, no pytest.

Run:  python3 proof_pure_batching.py
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

N = 200


def main() -> int:
    print("=== pure-test batching proof: no-fork fast path vs fork-per-test (real shim) ===\n")
    with tempfile.TemporaryDirectory() as root:
        body = "".join(f"def test_{i}():\n    assert {i} * 2 == {i * 2}\n" for i in range(N))
        body += "def test_impure():\n    globals().setdefault('_S', []).append(1)\n    assert True\n"
        with open(os.path.join(root, "test_pure.py"), "w") as f:
            f.write(body)
        sys.path.insert(0, root)
        shim._preimport(root)
        reg = shim._discover(root)
        engine = shim.Engine(reg, no_fork=False, root=root, purity_guard=True)

        nodes = [f"test_pure.py::test_{i}" for i in range(N)]

        # 1. Baseline: fork per test (isolated).
        t0 = time.perf_counter()
        fork_pass = sum(engine.run(n, "function", 5000)["outcome"] == "passed" for n in nodes)
        fork_ms = (time.perf_counter() - t0) * 1000

        # 2. Fast path: the SAME pure tests in-process (no fork).
        t0 = time.perf_counter()
        nofork_pass = sum(
            engine.run(n, "function", 5000, force_no_fork=True)["outcome"] == "passed" for n in nodes
        )
        nofork_ms = (time.perf_counter() - t0) * 1000

        # 3. Defense in depth: the impure test, even if run no-fork, is flagged pure=False.
        imp = engine.run("test_pure.py::test_impure", "function", 5000, force_no_fork=True)
        engine.teardown_all()

        speedup = fork_ms / nofork_ms if nofork_ms else 0
        print(f"    {N} pure tests, fork-per-test : {fork_pass}/{N} passed in {fork_ms:7.1f} ms")
        print(f"    {N} pure tests, NO fork       : {nofork_pass}/{N} passed in {nofork_ms:7.1f} ms")
        print(f"    speedup                      : {speedup:.0f}×")
        print(f"    impure test flagged          : pure={imp.get('pure')}  ({imp.get('impurity')})")

        go = (
            fork_pass == N
            and nofork_pass == N
            and speedup >= 5
            and imp.get("pure") is False
        )
        print(f"\n=== VERDICT: {'GO — pure tests run no-fork, same outcomes, big speedup; impure flagged' if go else 'NO-GO'} ===")
        return 0 if go else 1


if __name__ == "__main__":
    raise SystemExit(main())
