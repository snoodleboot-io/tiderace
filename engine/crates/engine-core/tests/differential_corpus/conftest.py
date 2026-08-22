"""Ancestor conftest — it sits ABOVE the run root on purpose (TID-19).

`tiderace run tests` walks from `tests/`, so a conftest here is only seen if ancestor
collection works. It also carries the two hooks a real suite puts at this level: a custom
option (TID-14's `request.config.getoption`) and a collection hook that skips marked tests
(TID-20).
"""

import os

import pytest

os.environ.setdefault("DIFFERENTIAL_CORPUS_ENV", "set-by-ancestor-conftest")


def pytest_addoption(parser):
    parser.addoption("--real", action="store_true", default=False, help="run backend tests")


def pytest_collection_modifyitems(config, items):
    if config.getoption("--real"):
        return
    for item in items:
        if "needs_backend" in item.keywords:
            item.add_marker(pytest.mark.skip(reason="pass --real to run backend tests"))


@pytest.fixture
def ancestor_fixture():
    """Declared above the run root; every test below must be able to request it."""
    return "from-ancestor"
