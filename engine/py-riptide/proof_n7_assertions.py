"""N7 proof — lazy assertion introspection / RichDiff (Phase 4, ADR-E009) through the REAL shim.
**No pytest.** Proves the signature pytest UX without pytest's import-time rewrite: a *failing* bare
`assert a == b` re-evaluates once to report operand values + a diff, while a *passing* assert pays
nothing. Also proves the purity fallback (a side-effecting assert degrades to the plain message, never
a misleading diff).

Run:  python3 proof_n7_assertions.py
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
    _SIDE = {"n": 0}

    def test_eq_ints():
        a, b = 2, 3
        assert a == b

    def test_eq_lists():
        assert [1, 2, 3] == [1, 9, 3]

    def test_eq_strings():
        assert "hello\\nworld" == "hello\\nthere"

    def test_eq_dicts():
        assert {"a": 1, "b": 2} == {"a": 1, "b": 3}

    def test_passes():
        assert 1 + 1 == 2

    def _bump():
        _SIDE["n"] += 1
        return _SIDE["n"]

    def test_side_effect():
        assert _bump() == 99   # re-eval would change the value → must fall back, not lie
    '''
)


def main() -> int:
    print("=== N7 proof: lazy assertion RichDiff through the real shim (ADR-E009, NO pytest) ===\n")
    with tempfile.TemporaryDirectory() as root:
        with open(os.path.join(root, "test_asserts.py"), "w") as f:
            f.write(CORPUS)
        sys.path.insert(0, root)
        shim._preimport(root)
        reg = shim._discover(root)
        engine = shim.Engine(reg, no_fork=True, root=root)

        def run(name):
            return engine.run(f"test_asserts.py::{name}", "function", 5000)

        checks = []

        r = run("test_eq_ints")
        ok = r["outcome"] == "failed" and "left  = 2" in r["detail"] and "right = 3" in r["detail"]
        checks.append(("ints: operand values reported", ok))
        print(f"[test_eq_ints]\n{_indent(r['detail'])}")

        r = run("test_eq_lists")
        ok = r["outcome"] == "failed" and "[1]" in r["detail"] and "2 != 9" in r["detail"]
        checks.append(("lists: per-element diff", ok))
        print(f"[test_eq_lists]\n{_indent(r['detail'])}")

        r = run("test_eq_strings")
        ok = r["outcome"] == "failed" and "world" in r["detail"] and "there" in r["detail"]
        checks.append(("strings: line diff", ok))

        r = run("test_eq_dicts")
        ok = r["outcome"] == "failed" and "b" in r["detail"] and "2 != 3" in r["detail"]
        checks.append(("dicts: per-key diff", ok))

        r = run("test_passes")
        ok = r["outcome"] == "passed" and r["detail"] == ""
        checks.append(("passing assert costs nothing (no diff)", ok))

        r = run("test_side_effect")
        # re-eval bumps to a different value → introspector must fall back to the plain message.
        ok = r["outcome"] == "failed" and "rich diff" not in r["detail"]
        checks.append(("side-effecting assert falls back (no misleading diff)", ok))

        engine.teardown_all()

        print("\n[checks]")
        for label, ok in checks:
            print(f"    {'ok' if ok else '!!':<3} {label}")
        go = all(ok for _, ok in checks)
        print(f"\n=== VERDICT: {'GO — lazy RichDiff works (rich on failure, free on pass, safe fallback)' if go else 'NO-GO'} ===")
        return 0 if go else 1


def _indent(text: str) -> str:
    return "\n".join("    " + ln for ln in text.strip().splitlines())


if __name__ == "__main__":
    raise SystemExit(main())
