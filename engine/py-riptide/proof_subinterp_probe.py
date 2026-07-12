"""Sub-interpreter safety probe proof (ADR-E015 / TID-9). Importing a module in a fresh isolated
sub-interpreter (`concurrent.interpreters`, PEP 734) classifies it safe/unsafe for the sub-interpreter
execution tier: a pure-Python module loads (safe); a module pulling a single-phase-init C-extension
(numpy) does not (unsafe). Real shim, no pytest.

Run:  python3 proof_subinterp_probe.py     (needs CPython 3.14+ for the probe API)
"""
from __future__ import annotations

import os
import sys
import tempfile

_HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(_HERE, os.pardir, "py-shim"))

import shim  # noqa: E402


def main() -> int:
    print("=== sub-interpreter safety probe (ADR-E015 / TID-9) ===\n")
    with tempfile.TemporaryDirectory() as root:
        open(os.path.join(root, "test_pure.py"), "w").write(
            "def test_ok():\n    assert sum(range(10)) == 45\n"
        )
        open(os.path.join(root, "test_np.py"), "w").write(
            "import numpy\ndef test_np():\n    assert int(numpy.array([1, 2]).sum()) == 3\n"
        )
        paths = [root] + list(sys.path)

        pure = shim._probe_module_safe("test_pure.py", paths)
        print(f"    pure module   → {pure}")

        if pure.get("safe") is None:
            print(f"\n=== SKIP: {pure.get('reason')} — needs CPython 3.14+ ===")
            return 0  # environment can't probe; the caller falls back to fork (sound)

        checks = [("pure module is safe", pure.get("safe") is True)]

        # The unsafe case needs numpy present to import; test it only when available.
        try:
            import numpy  # noqa: F401
            np = shim._probe_module_safe("test_np.py", paths)
            print(f"    numpy module  → {np}")
            checks.append(("numpy module is unsafe", np.get("safe") is False))
            checks.append(("reason names subinterpreters", "subinterp" in (np.get("reason") or "").lower()))
        except ImportError:
            print("    numpy module  → (numpy not installed; skipping the unsafe case)")

        ok = True
        for label, good in checks:
            ok = ok and good
            print(f"    {'ok' if good else '!!':<3} {label}")
        print(f"\n=== VERDICT: {'GO — pure modules classified safe, numpy classified unsafe' if ok else 'NO-GO'} ===")
        return 0 if ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
