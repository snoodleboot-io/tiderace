"""B3 proof — migration type-inference for untyped fixtures. Asserts both recall (the confident shapes
are inferred) and **precision** (ambiguous shapes are NEVER given a wrong annotation — they stay
flagged for the human). Pure `tiderace.migrate`, no pytest.

Run:  python3 proof_b3_inference.py
"""
from __future__ import annotations

import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from tiderace.migrate import migrate_source  # noqa: E402


def _inferred(body: str) -> str | None:
    """Return the inferred annotation for a single untyped fixture body, or None if flagged."""
    src = "import pytest\n\n@pytest.fixture\ndef fx():\n" + "\n".join("    " + ln for ln in body.splitlines())
    out, rep = migrate_source(src)
    # find `def fx() -> X:` in the migrated source
    for line in out.splitlines():
        if line.startswith("def fx()"):
            return line.split("->", 1)[1].rstrip(":").strip() if "->" in line else None
    return None


CASES = [
    # (body, expected inferred type or None)  — recall (Some) + precision (None)
    ("return Client(timeout=5)", "Client"),         # constructor call
    ("d = Db()\nyield d\nd.close()", "Db"),          # yield through a local assignment
    ("return mod.Engine()", "mod.Engine"),           # dotted constructor
    ("return {'a': 1}", "dict"),                      # literal dict
    ("return [1, 2, 3]", "list"),                     # literal list
    ("return 'hello'", "str"),                        # literal str
    ("return make_thing()", None),                    # lowercase factory ⇒ NOT inferred (precision)
    ("return some_var", None),                        # unresolved bare name ⇒ NOT inferred
    ("if cond:\n    return A()\nreturn B()", None),  # conflicting shapes ⇒ NOT inferred
]


def main() -> int:
    print("=== B3 proof: migration type-inference (recall + precision) ===\n")
    ok = True
    for body, expect in CASES:
        got = _inferred(body)
        passed = got == expect
        ok = ok and passed
        label = body.replace("\n", " ⏎ ")
        print(f"    {'ok' if passed else '!!':<3} {label:<45} → {got!r}  (want {expect!r})")
    print(f"\n=== VERDICT: {'GO — confident shapes inferred, ambiguous ones never mis-annotated' if ok else 'NO-GO'} ===")
    return 0 if ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
