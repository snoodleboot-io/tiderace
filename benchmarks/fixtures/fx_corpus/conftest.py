"""Root (session) conftest for fx_corpus.

Provides the per-run probe directory, the session-scoped fixtures, a
session-scoped AUTOUSE fixture (enters every test's closure without being
requested), and the session-level ``shared_value`` fixture that a leaf module
overrides by location (nearer-wins).
"""
import os
import sys
import tempfile

import pytest

sys.path.insert(0, os.path.dirname(__file__))
from fx_probe import setup, teardown  # noqa: E402


@pytest.fixture(scope="session")
def probe_dir():
    """Create a fresh per-run probe dir and expose it via env var.

    Yields the path; cleans up on session teardown. This is the ONLY fixture
    allowed to touch the filesystem outside the committed tree, keeping the
    corpus deterministic.
    """
    # Honor a caller-supplied dir (the Rust oracle / a debugging run sets
    # FX_CORPUS_PROBE_DIR to inspect events.log + counts.json); otherwise make a
    # fresh temp dir. Either way the value is exported so fx_probe can find it.
    d = os.environ.get("FX_CORPUS_PROBE_DIR") or tempfile.mkdtemp(
        prefix="fx_corpus_probe_"
    )
    os.makedirs(d, exist_ok=True)
    os.environ["FX_CORPUS_PROBE_DIR"] = d
    setup("probe_dir")
    yield d
    teardown("probe_dir")


@pytest.fixture(scope="session")
def session_db(probe_dir):
    """Session-scoped fixture: set up ONCE in the wellspring lineage.

    Stands in for an expensive session resource (Layer 2 / Watermark S). The
    scope-count probe must show this body ran exactly once across the suite.
    """
    setup("session_db")
    yield {"rows": 10_000}
    teardown("session_db")


@pytest.fixture(scope="session", autouse=True)
def session_autouse(probe_dir):
    """Session AUTOUSE fixture: injected into every in-scope test's closure
    without being requested by name."""
    setup("session_autouse")
    yield
    teardown("session_autouse")


@pytest.fixture(scope="session")
def shared_value(probe_dir):
    """Session definition of ``shared_value`` — OVERRIDDEN by location in
    pkg_override/test_override.py (module def must win there)."""
    setup("shared_value@session")
    yield "from-session-conftest"
    teardown("shared_value@session")
