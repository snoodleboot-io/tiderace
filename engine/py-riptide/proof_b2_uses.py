"""B2 proof — `@riptide.uses(Type)` (native `usefixtures`) through the REAL shim. **No pytest.**
A provider requested via `uses` is set up (and torn down) around the test for its side effects, by
TYPE, without being passed as a parameter.

Run:  python3 proof_b2_uses.py
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

    LOG = []

    class Seeded:
        pass

    @riptide.provides
    def seeded() -> Seeded:
        LOG.append("setup")
        yield Seeded()
        LOG.append("teardown")

    @riptide.uses(Seeded)
    def test_provider_ran_without_injection():     # NB: no parameters at all
        assert LOG == ["setup"]                     # the provider was set up before the body

    def test_no_uses_no_setup():
        assert LOG == ["setup", "teardown"]         # prior test's provider tore down; this one runs nothing
    '''
)


def main() -> int:
    print("=== B2 proof: @riptide.uses (native usefixtures) through the real shim (NO pytest) ===\n")
    with tempfile.TemporaryDirectory() as root:
        with open(os.path.join(root, "test_uses.py"), "w") as f:
            f.write(CORPUS)
        sys.path.insert(0, root)
        shim._preimport(root)
        reg = shim._discover(root)
        engine = shim.Engine(reg, no_fork=True, root=root)

        order = [
            ("test_provider_ran_without_injection", "passed"),
            ("test_no_uses_no_setup", "passed"),
        ]
        results = {}
        for name, want in order:
            res = engine.run(f"test_uses.py::{name}", "pytest_func", 5000)
            results[name] = res["outcome"]
            mark = "ok" if res["outcome"] == want else f"!! expected {want} ({res['detail'].strip()[-60:]})"
            print(f"    {name:<40} {res['outcome']:<8} {mark}")
        engine.teardown_all()

        go = all(results[n] == w for n, w in order)
        print(f"\n=== VERDICT: {'GO — uses-provider set up by type + torn down, not injected' if go else 'NO-GO'} ===")
        return 0 if go else 1


if __name__ == "__main__":
    raise SystemExit(main())
