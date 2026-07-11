"""B5 proof — provider-level parametrization (`@riptide.provides(params=...)`) through the REAL shim.
**No pytest.** A parametrized provider fans the test out, one run per param value, which the provider
reads via `request.param` — the native form of `@pytest.fixture(params=...)`.

Run:  python3 proof_b5_provider_params.py
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
    import riptide

    SEEN = []

    class Backend:
        def __init__(self, name):
            self.name = name

    @riptide.provides(params=["sqlite", "memory", "postgres"])
    def backend(request) -> Backend:        # fans out: one run per param; value via request.param
        return Backend(request.param)

    def test_runs_per_backend(b: Backend):  # wired by TYPE; runs 3× (once per provider param)
        SEEN.append(b.name)
        assert b.name in ("sqlite", "memory", "postgres")

    @riptide.provides(params=[1, 2])
    def n(request) -> int:
        return request.param

    def test_fails_on_one(v: int):          # passes for 1, fails for 2 → worst-wins = failed
        assert v == 1
    '''
)


def main() -> int:
    print("=== B5 proof: provider-level params (fan-out) through the real shim (NO pytest) ===\n")
    with tempfile.TemporaryDirectory() as root:
        with open(os.path.join(root, "test_pp.py"), "w") as f:
            f.write(CORPUS)
        sys.path.insert(0, root)
        shim._preimport(root)
        reg = shim._discover(root)
        engine = shim.Engine(reg, no_fork=True, root=root)

        r1 = engine.run("test_pp.py::test_runs_per_backend", "function", 5000)
        from test_pp import SEEN  # type: ignore  # noqa: E402

        r2 = engine.run("test_pp.py::test_fails_on_one", "function", 5000)
        engine.teardown_all()

        fanned = sorted(SEEN) == ["memory", "postgres", "sqlite"]
        print(f"    test_runs_per_backend  outcome={r1['outcome']}  seen={sorted(SEEN)}")
        print(f"    test_fails_on_one      outcome={r2['outcome']} (expect failed: param 2 fails)")

        go = r1["outcome"] == "passed" and fanned and r2["outcome"] == "failed"
        print(f"\n[checks]")
        print(f"    ok fanned out to all 3 provider params : {fanned}")
        print(f"    ok passing-across-params test passed   : {r1['outcome'] == 'passed'}")
        print(f"    ok one-bad-param folds to failed       : {r2['outcome'] == 'failed'}")
        print(f"\n=== VERDICT: {'GO — native provider params fan out via request.param' if go else 'NO-GO'} ===")
        return 0 if go else 1


if __name__ == "__main__":
    raise SystemExit(main())
