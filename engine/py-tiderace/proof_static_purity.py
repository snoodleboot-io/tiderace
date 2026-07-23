"""Static purity pre-filter proof — classify obvious mutators as impure WITHOUT running them (a sound
over-approximation: a static 'impure' only ever costs a fork; a 'no obvious impurity' is a no-fork
candidate the runtime guard confirms). Pure AST analysis.

Run:  python3 proof_static_purity.py
"""
from __future__ import annotations

import os
import random
import sys

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), os.pardir, "py-shim"))

from shim import static_impurity  # noqa: E402

_G = {"n": 0}


# ---- expected PURE (no obvious shared-state mutation → None) ----
def t_pure_local():
    x = sum(range(10))
    assert x == 45

def t_pure_reads_global():
    return _G["n"]              # reads, does not write

def t_pure_local_dict():
    d = {}
    d["x"] = 1                  # writes a LOCAL ⇒ fine
    assert d == {"x": 1}

def t_pure_local_augassign():
    x = 0
    x += 1
    assert x == 1


# ---- expected IMPURE (obvious shared-state mutation → reason) ----
def t_global_stmt():
    global _G
    _G = {"n": 9}

def t_subscript_global():
    _G["n"] += 1               # writes through a free/module name

def t_env_write():
    os.environ["X"] = "1"      # writes through `os` (non-local)

def t_chdir():
    os.chdir("/tmp")           # process-global call

def t_random_seed():
    random.seed(0)             # mutates global RNG state


CASES = [
    (t_pure_local, None),
    (t_pure_reads_global, None),
    (t_pure_local_dict, None),
    (t_pure_local_augassign, None),
    (t_global_stmt, "impure"),
    (t_subscript_global, "impure"),
    (t_env_write, "impure"),
    (t_chdir, "impure"),
    (t_random_seed, "impure"),
]


def main() -> int:
    print("=== static purity pre-filter proof (no run; AST only) ===\n")
    ok = True
    for func, want in CASES:
        reason = static_impurity(func)
        got_impure = reason is not None
        good = got_impure == (want == "impure")
        ok = ok and good
        verdict = f"impure ({reason})" if reason else "pure-candidate"
        print(f"    {'ok' if good else '!!':<3} {func.__name__:<26} → {verdict}")
    print(f"\n=== VERDICT: {'GO — obvious mutators flagged statically; clean tests pass through' if ok else 'NO-GO'} ===")
    return 0 if ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
