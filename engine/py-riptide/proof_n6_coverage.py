"""N6 proof — per-test coverage capture through the REAL engine shim (Phase 5 / ADR-E006). **No pytest.**

Proves the dependency tracker that the content-addressed cache (ADR-E004) and impact analysis
(design 11) are built on: with capture enabled, the shim returns each test's executed-source footprint
(`{rel_path: [lines]}`) — touching exactly the source a test exercised, and NOT the lines it didn't.
Uses `sys.monitoring` on 3.12+ (settrace below), scoped to the test inside its fork child.

Run:  python3 proof_n6_coverage.py
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

# Source under test — `used` is exercised, `unused` is not; coverage must tell them apart.
MYMOD = textwrap.dedent(
    '''
    def used(x):
        return x + 1

    def unused(x):
        return x - 1
    '''
)

TEST = textwrap.dedent(
    '''
    from mymod import used

    def test_uses_used():
        assert used(1) == 2
    '''
)


def main() -> int:
    print("=== N6 proof: per-test coverage capture through the real shim (ADR-E006, NO pytest) ===\n")
    with tempfile.TemporaryDirectory() as root:
        with open(os.path.join(root, "mymod.py"), "w") as f:
            f.write(MYMOD)
        with open(os.path.join(root, "test_cov.py"), "w") as f:
            f.write(TEST)

        sys.path.insert(0, root)
        shim._preimport(root)
        reg = shim._discover(root)

        mech = "sys.monitoring" if getattr(sys, "monitoring", None) else "sys.settrace"
        print(f"[mechanism] {mech} (CPython {sys.version_info.major}.{sys.version_info.minor})")

        engine = shim.Engine(reg, no_fork=True, root=root, coverage=True)
        res = engine.run("test_cov.py::test_uses_used", "function", 5000)
        engine.teardown_all()

        cov = res.get("coverage", {})
        print(f"[outcome]  {res['outcome']}")
        print(f"[coverage] {cov}")

        mymod = cov.get("mymod.py", [])
        # `return x + 1` is line 3 of mymod.py (after the leading blank from dedent); `return x - 1`
        # is line 6. Assert by content rather than guessing exact numbers, to be dedent-robust.
        src_lines = MYMOD.splitlines()
        used_line = next(i + 1 for i, ln in enumerate(src_lines) if "x + 1" in ln)
        unused_line = next(i + 1 for i, ln in enumerate(src_lines) if "x - 1" in ln)

        outcome_ok = res["outcome"] == "passed"
        touched_used = used_line in mymod
        skipped_unused = unused_line not in mymod
        touched_test = bool(cov.get("test_cov.py"))

        print(f"\n[checks]")
        print(f"    test passed                         : {outcome_ok}")
        print(f"    touched mymod.py L{used_line} (used)         : {touched_used}")
        print(f"    did NOT touch mymod.py L{unused_line} (unused)  : {skipped_unused}")
        print(f"    captured test_cov.py lines          : {touched_test}")

        go = outcome_ok and touched_used and skipped_unused and touched_test
        print(f"\n=== VERDICT: {'GO — precise per-test footprint captured through the real shim' if go else 'NO-GO'} ===")
        return 0 if go else 1


if __name__ == "__main__":
    raise SystemExit(main())
