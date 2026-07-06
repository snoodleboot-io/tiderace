"""Trusted-pure bare no-fork proof (TID-1). A test the caller has *recorded as pure* is run with
`trusted_pure=True`: the shim skips the snapshot/restore entirely (bare no-fork, ~90×) instead of
snapshot+restore (~5–14×). The purity guard/restore path still MEASURES and reports a `pure` verdict so
the recording can be built in the first place. Real shim, no pytest.

Run:  python3 proof_trusted_pure.py
"""
from __future__ import annotations

import os
import sys
import tempfile
import time

_HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, _HERE)
sys.path.insert(0, os.path.join(_HERE, os.pardir, "py-shim"))

import shim  # noqa: E402

N = 60
# A pure module (tests only read/compute) and an impure one (each test mutates a shared global).
PURE = "".join(f"def test_p{i}():\n    assert sum(range({i}+1)) >= 0\n" for i in range(N))
IMPURE = "_S = {'n': 0}\n" + "".join(
    f"def test_m{i}():\n    _S['n'] += 1\n    assert _S['n'] == 1\n" for i in range(N)
)


def one(engine, node, **kw):
    return engine.run(node, "function", 5000, **kw)


def main() -> int:
    print("=== trusted-pure bare no-fork proof (TID-1) ===\n")
    with tempfile.TemporaryDirectory() as root:
        open(os.path.join(root, "test_pure.py"), "w").write(PURE)
        open(os.path.join(root, "test_impure.py"), "w").write(IMPURE)
        sys.path.insert(0, root)
        shim._preimport(root)
        reg = shim._discover(root)
        e = shim.Engine(reg, no_fork=False, root=root, restore=True)

        pure_nodes = [f"test_pure.py::test_p{i}" for i in range(N)]
        impure_nodes = [f"test_impure.py::test_m{i}" for i in range(N)]

        # 1. Restore mode MEASURES purity: pure → pure=True, impure → pure=False (and undoes the mutation).
        rp = one(e, pure_nodes[0], force_no_fork=True)
        ri = one(e, impure_nodes[0], force_no_fork=True)
        measured_pure = rp.get("pure") is True
        measured_impure = ri.get("pure") is False

        # 2. Trusted-pure SKIPS the snapshot (bare): no `pure` measured, outcome still correct.
        rt = one(e, pure_nodes[1], force_no_fork=True, trusted_pure=True)
        bare_unmeasured = "pure" not in rt and rt["outcome"] == "passed"

        # 3. Bare (trusted) is cheaper than snapshot+restore over the pure suite.
        t0 = time.perf_counter()
        for n in pure_nodes:
            one(e, n, force_no_fork=True)  # snapshot each (restore path)
        restore_ms = (time.perf_counter() - t0) * 1000
        t0 = time.perf_counter()
        for n in pure_nodes:
            one(e, n, force_no_fork=True, trusted_pure=True)  # bare
        bare_ms = (time.perf_counter() - t0) * 1000
        e.teardown_all()

        print(f"    restore mode measures pure test  → pure=True   : {measured_pure}")
        print(f"    restore mode measures impure test → pure=False  : {measured_impure}")
        print(f"    trusted_pure skips snapshot (no 'pure', passed) : {bare_unmeasured}")
        print(f"    {N} pure tests: snapshot/restore {restore_ms:6.1f} ms  vs  bare {bare_ms:6.1f} ms "
              f"({restore_ms / max(bare_ms, 0.001):.1f}× cheaper)")

        go = measured_pure and measured_impure and bare_unmeasured and bare_ms < restore_ms
        print(f"\n=== VERDICT: {'GO — verdict measured under restore; trusted_pure runs bare & cheaper' if go else 'NO-GO'} ===")
        return 0 if go else 1


if __name__ == "__main__":
    raise SystemExit(main())
