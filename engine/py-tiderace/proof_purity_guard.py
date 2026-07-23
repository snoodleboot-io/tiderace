"""Purity-guard proof — the prerequisite for pure-test batching. The shim, with the guard on, classifies
each test as **pure** (didn't mutate shared state → safe to run without a fork) or **impure** (mutated a
module global / env → must be isolated). Drives the real shim, no pytest.

Run:  python3 proof_purity_guard.py
"""
from __future__ import annotations

import os
import sys
import tempfile
import textwrap

_HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, _HERE)
sys.path.insert(0, os.path.join(_HERE, os.pardir, "py-shim"))

import shim  # noqa: E402

CORPUS = textwrap.dedent(
    '''
    import os

    _COUNTER = {"n": 0}
    _LIST = []

    def test_pure_local():            # only locals + assert ⇒ PURE
        x = sum(range(10))
        assert x == 45

    def test_pure_reads_global():     # reads but does not mutate ⇒ PURE
        assert _COUNTER["n"] == 0

    def test_mutates_dict_global():   # in-place mutation of a module dict ⇒ IMPURE
        _COUNTER["n"] += 1
        assert _COUNTER["n"] == 1

    def test_mutates_list_global():   # appends to a module list ⇒ IMPURE
        _LIST.append(1)
        assert _LIST == [1]

    def test_mutates_env():           # mutates os.environ ⇒ IMPURE
        os.environ["TIDERACE_PURITY_PROBE"] = "x"
        assert os.environ["TIDERACE_PURITY_PROBE"] == "x"
    '''
)


def main() -> int:
    print("=== purity-guard proof: classify pure vs impure tests through the real shim ===\n")
    with tempfile.TemporaryDirectory() as root:
        with open(os.path.join(root, "test_purity.py"), "w") as f:
            f.write(CORPUS)
        sys.path.insert(0, root)
        shim._preimport(root)
        reg = shim._discover(root)
        engine = shim.Engine(reg, no_fork=False, root=root, purity_guard=True)

        expected = {
            "test_pure_local": True,
            "test_pure_reads_global": True,
            "test_mutates_dict_global": False,
            "test_mutates_list_global": False,
            "test_mutates_env": False,
        }
        ok = True
        for name, want_pure in expected.items():
            res = engine.run(f"test_purity.py::{name}", "function", 5000)
            pure = res.get("pure")
            good = (pure == want_pure) and res["outcome"] == "passed"
            ok = ok and good
            reason = f"  ({res.get('impurity')})" if res.get("impurity") else ""
            mark = "ok" if good else f"!! want pure={want_pure}"
            print(f"    {name:<26} pure={str(pure):<5} {mark}{reason}")
        engine.teardown_all()

        print(f"\n=== VERDICT: {'GO — pure/impure classified correctly (batchable tests identified)' if ok else 'NO-GO'} ===")
        return 0 if ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
