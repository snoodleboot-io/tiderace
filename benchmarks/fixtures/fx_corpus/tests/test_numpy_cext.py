"""Real C-extension boundary: a session-scoped fixture imports and uses numpy,
so the wellspring snapshot is taken with a real native extension already warm in
memory (the E-2 fork-from-warm hazard). Under plain pytest this just passes."""
import os
import sys

import numpy as np
import pytest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))
from fx_probe import setup, teardown  # noqa: E402


@pytest.fixture(scope="module")
def warm_array(session_db):
    """Module-scoped numpy array built once; snapshotted with numpy warm."""
    setup("warm_array")
    arr = np.arange(1000, dtype=np.int64)
    yield arr
    teardown("warm_array")


def test_numpy_sum(warm_array):
    assert int(warm_array.sum()) == (999 * 1000) // 2


def test_numpy_shape(warm_array):
    assert warm_array.shape == (1000,)
