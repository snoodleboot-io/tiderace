"""Migrate conformance (TID-8): a pytest fixture that yields/returns NO value is a pure setup/teardown
fixture — it provides `None`, so `riptide migrate` annotates it `-> None` (mapped) instead of flagging
it "untyped". A fixture that DOES return a value it can't type is still flagged (precision preserved).

Run:  python3 proof_migrate_novalue.py
"""
from __future__ import annotations

import ast
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from riptide.migrate import migrate_source  # noqa: E402

SRC = '''import pytest

@pytest.fixture(autouse=True)
def reset_env():           # bare yield → setup/teardown, provides None
    old = dict(os.environ)
    yield
    os.environ.update(old)

@pytest.fixture
def setup_only():          # no return/yield at all → provides None
    do_setup()

@pytest.fixture
def db():                  # returns a lowercase factory call → un-inferable, must stay flagged
    return make_db()

@pytest.fixture
def client():              # returns a Capitalized ctor → inferred (existing B3), not None
    return Client()
'''


def main() -> int:
    print("=== migrate: no-value fixture → `-> None` (TID-8) ===\n")
    out, rep = migrate_source(SRC)
    ast.parse(out)  # migrated source must be syntactically valid

    novalue = {m.message.split("`")[1] for m in rep.mappings if "no value" in m.message}
    cant = {c.message.split("`")[1] for c in rep.cant_map if "has no return type" in c.message}
    inferred = {m.message.split("`")[1] for m in rep.mappings if "inferred" in m.message}

    checks = [
        ("reset_env → -> None (bare yield)", "reset_env" in novalue),
        ("setup_only → -> None (no return)", "setup_only" in novalue),
        ("db still flagged (lowercase factory)", "db" in cant),
        ("client inferred -> Client (not None)", "client" in inferred and "client" not in novalue),
        ("-> None present in output", "-> None" in out),
    ]
    ok = True
    for label, good in checks:
        ok = ok and good
        print(f"    {'ok' if good else '!!':<3} {label}")
    print(f"\n=== VERDICT: {'GO — setup/teardown fixtures map to -> None; typed inference unchanged' if ok else 'NO-GO'} ===")
    return 0 if ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
