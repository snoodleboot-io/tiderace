"""Override-by-location: this module defines ``shared_value`` at module scope,
shadowing the session-conftest definition of the same name. Nearer wins, so the
test here must resolve to the module value, never the conftest value."""
import os
import sys

import pytest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", ".."))
from fx_probe import setup, teardown  # noqa: E402


@pytest.fixture
def shared_value():
    """Module-level override of the session-conftest ``shared_value``."""
    setup("shared_value@module")
    yield "from-module-override"
    teardown("shared_value@module")


def test_override_wins(shared_value):
    # If override-by-location is correct, the MODULE definition wins here.
    assert shared_value == "from-module-override"
