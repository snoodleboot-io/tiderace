"""N5b proof — the `caplog` builtin (TID-14) driven through the REAL engine shim. **No pytest.**

Sibling of `proof_n5_builtins.py`, which covers monkeypatch / tmp_path / capsys / capfd. `caplog`
lives in `tiderace.builtins` too, so it cannot be exercised from the Rust acceptance suite: the
`.tiderace-fx-venv` those tests drive has pytest and numpy but no `tiderace` installed, so the
shim's `_register_builtins` silently no-ops there. This proof puts the package on `sys.path`
directly, the same way `proof_n5_builtins.py` does.

Proves, decisively:

  • **name-DI and type-DI both wire** — `caplog` (the pytest spelling) and `log: CapLog` (the
    migrated spelling) reach the same provider;
  • **`record.message` is populated** — `logging` only sets that attribute when a Formatter formats
    the record, and `any("x" in rec.message for rec in caplog.records)` is the single most common
    caplog assertion, so a handler that merely appends raises `AttributeError`;
  • **`at_level` restores the prior level**, and `set_level`'s change is undone at teardown — run in
    `no_fork=True` so the leak is observable in this one process. Under fork each child is a fresh
    COW copy, which would mask the bug rather than prove its absence;
  • **the handler is removed at teardown** — a leaked handler would keep capturing into a dead
    fixture's list for the rest of the session.

Run:  python3 proof_n5b_caplog.py
"""
from __future__ import annotations

import os
import sys
import tempfile
import textwrap

_HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, _HERE)  # the `tiderace` package (so the shim can `import tiderace.builtins`)
sys.path.insert(0, os.path.join(_HERE, os.pardir, "py-shim"))  # `shim`

import shim  # noqa: E402

CORPUS = textwrap.dedent(
    '''
    import logging

    from tiderace.builtins import CapLog

    LEAK = {}  # carries observations to the next test, to prove teardown

    def test_records_are_captured(caplog):          # pytest spelling → BY NAME
        logging.getLogger("proof").warning("hello %s", "world")
        assert len(caplog.records) == 1

    def test_record_message_is_populated(caplog):   # the assertion shape caplog tests actually use
        logging.getLogger("proof").warning("span finish")
        assert any("span finish" in rec.message for rec in caplog.records)

    def test_by_type_also_wires(log: CapLog):       # `log` != provider name "caplog" → BY TYPE
        logging.getLogger("proof").error("typed")
        assert log.messages == ["typed"]

    def test_at_level_captures_debug(caplog):
        logger = logging.getLogger("proof.debug")
        LEAK["before"] = logger.level
        with caplog.at_level(logging.DEBUG, logger=logger.name):
            logger.debug("span event")
        assert any("span event" in rec.message for rec in caplog.records)
        LEAK["after"] = logger.level               # at_level must have restored it on exit

    def test_at_level_restored_on_exit():
        assert LEAK["before"] == LEAK["after"], "at_level leaked a level change"

    def test_set_level_is_undone_at_teardown(caplog):
        logger = logging.getLogger("proof.setlevel")
        LEAK["set_before"] = logger.level
        caplog.set_level(logging.DEBUG, logger=logger.name)
        assert logger.level == logging.DEBUG

    def test_set_level_restored():                  # teardown of the prior test must have undone it
        logger = logging.getLogger("proof.setlevel")
        assert logger.level == LEAK["set_before"]

    def test_handler_removed_at_teardown():
        root = logging.getLogger()
        leaked = [h for h in root.handlers if type(h).__name__ == "_Recorder"]
        assert not leaked, f"caplog left {len(leaked)} handler(s) on the root logger"

    def test_records_do_not_leak_between_tests(caplog):
        # A fresh CapLog per test: nothing any earlier test logged may appear here.
        assert caplog.records == []
        assert caplog.text == ""
    '''
)


def main() -> int:
    print("=== N5b proof: the caplog builtin through the real shim (NO pytest) ===\n")
    with tempfile.TemporaryDirectory() as root:
        with open(os.path.join(root, "test_caplog.py"), "w") as f:
            f.write(CORPUS)

        sys.path.insert(0, root)
        shim._preimport(root)
        reg = shim._discover(root)

        registered_ok = "caplog" in reg.by_name
        typed = {t.__name__: ns for t, ns in reg.by_type.items()}
        print(f"[discovery] caplog registered by name : {registered_ok}")
        print(f"[discovery] CapLog indexed by type    : {typed.get('CapLog')}")
        registered_ok = registered_ok and typed.get("CapLog") == ["caplog"]

        # `no_fork=True`: teardown effects have to be observable in THIS process for the
        # restore/cleanup assertions to mean anything.
        engine = shim.Engine(reg, no_fork=True)
        order = [
            "test_records_are_captured",
            "test_record_message_is_populated",
            "test_by_type_also_wires",
            "test_at_level_captures_debug",
            "test_at_level_restored_on_exit",
            "test_set_level_is_undone_at_teardown",
            "test_set_level_restored",
            "test_handler_removed_at_teardown",
            "test_records_do_not_leak_between_tests",
        ]
        results = {}
        print("\n[run]")
        for name in order:
            res = engine.run(f"test_caplog.py::{name}", "function", 5000)
            results[name] = res["outcome"]
            mark = "ok" if res["outcome"] == "passed" else "!! expected passed"
            detail = f"  ({res['detail'].strip().splitlines()[-1]})" if res["detail"] else ""
            print(f"    {name:<38} {res['outcome']:<8} {mark}{detail}")
        engine.teardown_all()

        go = registered_ok and all(results[n] == "passed" for n in order)
        print(f"\n=== VERDICT: {'GO — caplog wires by name AND type, captures, and tears down cleanly' if go else 'NO-GO'} ===")
        return 0 if go else 1


if __name__ == "__main__":
    raise SystemExit(main())
