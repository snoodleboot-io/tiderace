"""Non-fork-safe resource boundary (reinit_after_fork).

``sqlite3.connect(":memory:")`` yields a connection that does NOT survive fork()
intact: a forked child must open a FRESH connection, never reuse the parent's
inherited handle. Intent is declared two ways the Rust engine can detect:

  1. Naming convention: the fixture name is prefixed ``reinit_after_fork__``.
  2. Marker: tests using it carry ``@pytest.mark.reinit_after_fork``.

Under plain pytest (no fork) this simply passes; the Rust differential oracle
uses the markers to assert each forked child gets a distinct connection.
"""
import os
import sqlite3
import sys

import pytest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))
from fx_probe import setup, teardown  # noqa: E402


@pytest.fixture
def reinit_after_fork__db_conn():
    """A non-fork-safe in-memory sqlite connection.

    Declared scope is function here, but the engine treats the connection HANDLE
    as reinit-in-child regardless of declared scope (split-setup). The
    ``reinit_after_fork__`` name prefix is the detectable intent declaration.
    """
    setup("reinit_after_fork__db_conn")
    conn = sqlite3.connect(":memory:")
    conn.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT)")
    conn.execute("INSERT INTO t (v) VALUES ('alpha'), ('beta')")
    conn.commit()
    yield conn
    conn.close()
    teardown("reinit_after_fork__db_conn")


@pytest.mark.reinit_after_fork
def test_sqlite_resource(reinit_after_fork__db_conn):
    rows = reinit_after_fork__db_conn.execute(
        "SELECT v FROM t ORDER BY id"
    ).fetchall()
    assert [r[0] for r in rows] == ["alpha", "beta"]


@pytest.mark.reinit_after_fork
def test_sqlite_fresh_per_test(reinit_after_fork__db_conn):
    # A second test re-acquires a fresh connection with the same seed data.
    count = reinit_after_fork__db_conn.execute("SELECT COUNT(*) FROM t").fetchone()
    assert count[0] == 2
