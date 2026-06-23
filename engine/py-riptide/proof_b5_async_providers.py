"""B5 (async providers) proof — `async def @riptide.provides` through the REAL shim. **No pytest.**
An async provider (coroutine or async-generator with teardown) is set up and torn down on the SAME
event loop as the (async or sync) test body, wired by TYPE.

Run:  python3 proof_b5_async_providers.py
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
    import asyncio
    import riptide

    LOG = []

    class Conn:
        def __init__(self):
            self.open = True

    @riptide.provides
    async def conn() -> Conn:                 # async-generator provider with async teardown
        await asyncio.sleep(0)
        c = Conn()
        LOG.append("setup")
        yield c
        await asyncio.sleep(0)
        c.open = False
        LOG.append("teardown")

    @riptide.provides
    async def token() -> str:                 # plain coroutine provider (no teardown)
        await asyncio.sleep(0)
        return "tok"

    async def test_async_body_async_fixture(c: Conn, t: str):
        await asyncio.sleep(0)
        assert c.open and t == "tok"

    def test_sync_body_async_fixture(c: Conn):   # sync test, async fixture → still driven on a loop
        assert c.open

    def test_teardown_ran():
        assert LOG.count("setup") == LOG.count("teardown") and LOG.count("teardown") >= 1
    '''
)


def main() -> int:
    print("=== B5 proof: async providers through the real shim (NO pytest) ===\n")
    with tempfile.TemporaryDirectory() as root:
        with open(os.path.join(root, "test_ap.py"), "w") as f:
            f.write(CORPUS)
        sys.path.insert(0, root)
        shim._preimport(root)
        reg = shim._discover(root)
        engine = shim.Engine(reg, no_fork=True, root=root)

        order = [
            ("test_async_body_async_fixture", "passed"),
            ("test_sync_body_async_fixture", "passed"),
            ("test_teardown_ran", "passed"),
        ]
        results = {}
        for name, want in order:
            res = engine.run(f"test_ap.py::{name}", "pytest_func", 5000)
            results[name] = res["outcome"]
            mark = "ok" if res["outcome"] == want else f"!! expected {want} ({res['detail'].strip()[-70:]})"
            print(f"    {name:<34} {res['outcome']:<8} {mark}")
        engine.teardown_all()

        go = all(results[n] == w for n, w in order)
        print(f"\n=== VERDICT: {'GO — async providers set up + torn down on one loop, wired by type' if go else 'NO-GO'} ===")
        return 0 if go else 1


if __name__ == "__main__":
    raise SystemExit(main())
