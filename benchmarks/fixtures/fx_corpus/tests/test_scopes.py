"""Exercise module / class / function scopes with observable yield-teardown
ordering. Setup order (per class test) is:
    session_db (once) -> pkg_resource (once) -> module_fix (once)
      -> class_fix (once per class) -> func_fix (per test)
Teardown must be the strict reverse at each scope boundary.
"""
import os
import sys

import pytest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))
from fx_probe import setup, teardown  # noqa: E402


@pytest.fixture(scope="module")
def module_fix(pkg_resource):
    """Module-scoped: body runs ONCE for this module regardless of test count."""
    setup("module_fix")
    yield {"module": True}
    teardown("module_fix")


@pytest.fixture(scope="class")
def class_fix(module_fix):
    """Class-scoped: body runs once per class."""
    setup("class_fix")
    yield {"class": True}
    teardown("class_fix")


@pytest.fixture
def func_fix(class_fix):
    """Function-scoped (default): body runs per test, torn down per test."""
    setup("func_fix")
    yield {"func": True}
    teardown("func_fix")


class TestScoped:
    """Two tests in one class share class_fix (once) but get fresh func_fix."""

    def test_a(self, func_fix):
        assert func_fix["func"] is True

    def test_b(self, func_fix):
        assert func_fix["func"] is True


class TestScopedAgain:
    """A second class -> class_fix body runs a second time, module_fix does not."""

    def test_c(self, func_fix):
        assert func_fix["func"] is True
