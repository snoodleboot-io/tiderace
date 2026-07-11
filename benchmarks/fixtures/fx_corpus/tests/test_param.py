"""Parametrized fixture: params=[...] fans out into one instance per value.
Each value drives a distinct test invocation (and, on the Rust side, a distinct
closure_hash)."""
import os
import sys

import pytest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))
from fx_probe import setup, teardown  # noqa: E402

PARAM_VALUES = ['a', 'b', 'c']


@pytest.fixture(params=PARAM_VALUES)
def parametrized(request):
    """Fans out into len(PARAM_VALUES) instances; body runs once per param."""
    value = request.param
    setup("parametrized[%s]" % value)
    yield value
    teardown("parametrized[%s]" % value)


def test_param(parametrized):
    assert parametrized in PARAM_VALUES
