"""N3 proof — riptide-native providers driven through the REAL engine shim (`engine/py-shim/shim.py`),
no pytest, no fork. Decisive on type-DI: the provider is named `database`, but tests request it as
`store: Db` / providers request it as `conn: Db` — **no name matches**, so a pass can only happen if the
shim resolved by TYPE. Drives `shim.Engine(..., no_fork=True)` over a temp native corpus.
"""
from __future__ import annotations

import os
import sys
import tempfile
import textwrap

_HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, _HERE)  # the `riptide` package
sys.path.insert(0, os.path.join(_HERE, os.pardir, "py-shim"))  # `shim`

import shim  # noqa: E402

CORPUS = textwrap.dedent(
    '''
    import riptide

    class Db:
        def __init__(self):
            self.rows = []
        def add(self, x):
            self.rows.append(x)
        def count(self):
            return len(self.rows)

    class Repo:
        def __init__(self, db: Db):
            self.db = db

    @riptide.provides(scope="module", name="database")   # provider name is "database"...
    def db() -> Db:
        d = Db()
        yield d

    @riptide.provides
    def repo(conn: Db) -> Repo:        # ...requested here as `conn: Db` — wired by TYPE, not name
        return Repo(conn)

    def test_by_type(store: Db):       # param `store` (no provider named "store") → type Db → "database"
        store.add("x")
        assert store.count() >= 1

    def test_provider_chain(r: Repo):  # r: Repo → "repo", whose Db dep → "database" (chained type-DI)
        assert isinstance(r.db, Db)

    def test_plain():
        assert 1 + 1 == 2

    @riptide.skip(reason="wip")
    def test_skipped(store: Db):       # would touch a fixture, but skip short-circuits before setup
        raise AssertionError("must not run")

    @riptide.skip_if(True, reason="env")
    def test_skip_if():
        raise AssertionError("must not run")

    @riptide.xfail(reason="known bug")
    def test_xfails():
        assert 1 == 2                  # fails → folds to xfail

    @riptide.xfail(reason="fixed now", strict=True)
    def test_xpass_strict():
        assert True                    # unexpected pass under strict → failed

    @riptide.cases([(2, 3, 5), (0, 0, 0)])
    def test_add(a, b, exp):           # bare params filled by @cases (no fixtures)
        assert a + b == exp

    @riptide.cases([(1,), (2,)])
    def test_cases_plus_fixture(n, store: Db):   # @cases AND a type-DI fixture together
        store.add(n)
        assert store.count() >= 1
    '''
)


def main() -> int:
    print("=== N3 proof: native providers through the real shim (type-DI, no pytest, no fork) ===\n")
    with tempfile.TemporaryDirectory() as root:
        with open(os.path.join(root, "test_native.py"), "w") as f:
            f.write(CORPUS)

        sys.path.insert(0, root)
        shim._preimport(root)
        reg = shim._discover(root)

        # The shim discovered native providers and indexed them BY TYPE (not pytest markers).
        from test_native import Db, Repo  # type: ignore  # noqa: E402

        print(f"[discovery] providers by name : {sorted(reg.by_name)}")
        print(f"[discovery] providers by type : "
              f"{{ {', '.join(f'{t.__name__}->{ns}' for t, ns in reg.by_type.items())} }}")
        type_di_ok = reg.by_type.get(Db) == ["database"] and reg.by_type.get(Repo) == ["repo"]

        engine = shim.Engine(reg, no_fork=True)
        # (node, expected outcome) — type-DI passes + the four native marks.
        expected = {
            "test_by_type": "passed",
            "test_provider_chain": "passed",
            "test_plain": "passed",
            "test_skipped": "skipped",
            "test_skip_if": "skipped",
            "test_xfails": "xfail",
            "test_xpass_strict": "failed",  # strict xpass
            "test_add": "passed",           # both cases pass (worst-wins aggregation)
            "test_cases_plus_fixture": "passed",  # @cases value + type-DI fixture together
        }
        results = {}
        print("\n[run]")
        for name, want in expected.items():
            res = engine.run(f"test_native.py::{name}", "pytest_func", 5000)
            results[name] = res["outcome"]
            mark = "ok" if res["outcome"] == want else f"!! expected {want}"
            detail = f"  ({res['detail'].strip().splitlines()[-1]})" if res["detail"] else ""
            print(f"    {name:<22} {res['outcome']:<8} {mark}{detail}")
        engine.teardown_all()

        go = type_di_ok and all(results[n] == w for n, w in expected.items())
        print(f"\n=== VERDICT: {'GO — native providers resolved BY TYPE through the real shim' if go else 'NO-GO'} ===")
        return 0 if go else 1


if __name__ == "__main__":
    raise SystemExit(main())
