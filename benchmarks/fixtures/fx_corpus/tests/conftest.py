"""Package conftest for the tests package: a package-scoped fixture (Layer 2.5,
between Session and Module) plus a package-scoped autouse fixture. Set up once
per package path."""
import os
import sys

import pytest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))
from fx_probe import setup, teardown  # noqa: E402


@pytest.fixture(scope="package")
def pkg_resource(session_db):
    """Package-scoped: depends on the wider session_db (scope-monotone:
    package may depend on session)."""
    setup("pkg_resource")
    yield {"pkg": True, "rows": session_db["rows"]}
    teardown("pkg_resource")


@pytest.fixture(scope="package", autouse=True)
def pkg_autouse():
    """A second autouse fixture, at package scope, to exercise multi-level
    autouse injection."""
    setup("pkg_autouse")
    yield
    teardown("pkg_autouse")
