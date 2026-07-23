"""N8 proof — async tests + unittest fidelity (Phase 4) through the REAL shim. **No pytest.**

  • `async def test_*` is driven to completion on a per-test event loop (pass/fail map correctly);
  • unittest: `setUpClass`/`tearDownClass` are honored (which `TestCase.run()` alone does NOT call),
    and `@expectedFailure` → xfail, an unexpected success → failed, a failing `subTest` → failed.

Run:  python3 proof_n8_async_unittest.py
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
    import unittest

    async def test_async_pass():
        await asyncio.sleep(0)
        assert 1 + 1 == 2

    async def test_async_fail():
        await asyncio.sleep(0)
        assert 1 + 1 == 3

    class T(unittest.TestCase):
        @classmethod
        def setUpClass(cls):
            cls.value = 42          # TestCase.run() alone never calls this

        def test_uses_class_setup(self):
            self.assertEqual(self.value, 42)   # AttributeError unless setUpClass ran

        @unittest.expectedFailure
        def test_expected_fail(self):
            self.assertEqual(1, 2)             # fails as expected → xfail

        @unittest.expectedFailure
        def test_unexpected_pass(self):
            self.assertEqual(1, 1)             # passes though marked xfail → failed

        def test_subtest(self):
            for i in (1, 2, 3):
                with self.subTest(i=i):
                    self.assertLess(i, 3)      # i=3 fails → failed
    '''
)


def main() -> int:
    print("=== N8 proof: async tests + unittest fidelity through the real shim (Phase 4, NO pytest) ===\n")
    with tempfile.TemporaryDirectory() as root:
        with open(os.path.join(root, "test_au.py"), "w") as f:
            f.write(CORPUS)
        sys.path.insert(0, root)
        shim._preimport(root)
        reg = shim._discover(root)
        engine = shim.Engine(reg, no_fork=True, root=root)

        expected = [
            ("test_au.py::test_async_pass", "function", "passed"),
            ("test_au.py::test_async_fail", "function", "failed"),
            ("test_au.py::T::test_uses_class_setup", "unittest_method", "passed"),
            ("test_au.py::T::test_expected_fail", "unittest_method", "xfail"),
            ("test_au.py::T::test_unexpected_pass", "unittest_method", "failed"),
            ("test_au.py::T::test_subtest", "unittest_method", "failed"),
        ]
        results = {}
        print("[run]")
        for node, style, want in expected:
            res = engine.run(node, style, 5000)
            results[node] = res["outcome"]
            mark = "ok" if res["outcome"] == want else f"!! expected {want}"
            label = node.split("::", 1)[1]
            print(f"    {label:<26} {res['outcome']:<8} {mark}")
        engine.teardown_all()

        go = all(results[n] == w for n, _, w in expected)
        print(f"\n=== VERDICT: {'GO — async + unittest fidelity correct through the real shim' if go else 'NO-GO'} ===")
        return 0 if go else 1


if __name__ == "__main__":
    raise SystemExit(main())
