"""Marks in one dialect (TID-123, step 1): both spellings normalise to one `Mark`, skips and xfails
fold the same way, and the marker API iterates closest first."""
from __future__ import annotations

import os
import sys
from types import SimpleNamespace

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(HERE))

from tiderace_shim import pytest_compat as pc  # noqa: E402


def pytest_mark(name, *args, **kwargs):
    """What `@pytest.mark.<name>(*args, **kwargs)` leaves on a function."""
    return SimpleNamespace(name=name, args=args, kwargs=kwargs)


def native(kind, reason="", condition=True, strict=False, name=""):
    """tiderace's own `_spec.Mark`, by shape."""
    return SimpleNamespace(kind=kind, reason=reason, condition=condition, strict=strict, name=name)


def test_both_dialects_normalise_to_one_mark():
    m = pc.normalise(pytest_mark("skipif", False, reason="no db"))
    assert (m.kind, m.name, m.reason, m.condition, m.default_reason) == ("skip_if", "skipif", "no db", False, "skipif")
    assert pc.normalise(pytest_mark("skip", "legacy")).reason == "legacy"  # a positional reason
    x = pc.normalise(pytest_mark("xfail", strict=True))
    assert (x.kind, x.condition, x.strict) == ("xfail", True, True)
    # A string first argument is not read as a condition: the xfail is taken as applying, as before.
    assert pc.normalise(pytest_mark("xfail", "sys.platform == 'win32'")).condition is True
    n = pc.normalise(native("skip_if", "slow box", condition=True))
    assert (n.kind, n.name, n.reason, n.default_reason) == ("skip_if", "skip_if", "slow box", "skip_if")
    assert pc.normalise(native("tag", name="slow")).name == "slow"
    assert pc.normalise(pytest_mark("usefixtures", "db")).source.args == ("db",)
    assert pc.normalise(n) is n or pc.normalise(pc.normalise(n)) == pc.normalise(n)


def test_skip_reason_takes_the_closest_applying_mark_and_leaves_string_conditions_alone():
    marks = pc.normalise_all([
        pytest_mark("skipif", "sys.platform == 'win32'", reason="never evaluated"),  # closest
        pytest_mark("skipif", False, reason="not this one"),
        pytest_mark("skipif", True, reason="this one"),
        pytest_mark("skip", reason="farthest"),
    ])
    assert pc.skip_reason(marks) == "this one"
    assert pc.skip_reason(pc.normalise_all([pytest_mark("skipif", True)])) == "skipif"
    assert pc.skip_reason(pc.normalise_all([pytest_mark("skip")])) == "skip"
    assert pc.skip_reason(pc.normalise_all([native("skip_if", condition=False)])) is None
    assert pc.skip_reason(pc.normalise_all([native("skip_if", condition=True)])) == "skip_if"
    assert pc.skip_reason(pc.normalise_all([native("skip", "because")])) == "because"
    assert pc.skip_reason([]) is None


def test_xfail_folds_as_before_in_both_dialects():
    for xf in (pc.normalise(pytest_mark("xfail", reason="known")), pc.normalise(native("xfail", "known"))):
        assert pc.fold([xf], "failed", "assert 1 == 2") == ("xfail", "known")
        assert pc.fold([xf], "error", "boom") == ("xfail", "known")
        assert pc.fold([xf], "passed", "") == ("xpass", "known")
        assert pc.fold([xf], "skipped", "s") == ("skipped", "s")
    bare = pc.normalise(pytest_mark("xfail"))
    assert pc.fold([bare], "failed", "assert 1 == 2") == ("xfail", "assert 1 == 2")  # the detail stands in
    strict = pc.normalise(pytest_mark("xfail", reason="r", strict=True))
    assert pc.fold([strict], "passed", "") == ("failed", "[xpass strict] r")
    assert pc.fold([pc.normalise(native("xfail", strict=True))], "passed", "") == ("failed", "[xpass strict]")
    off = pc.normalise(pytest_mark("xfail", False, reason="off"))
    assert pc.fold([off], "failed", "d") == ("failed", "d")
    assert pc.fold([pc.normalise(pytest_mark("xfail", "cond-text"))], "failed", "d") == ("xfail", "d")


def test_fold_takes_the_closest_xfail_and_a_runtime_skip_wins():
    closest = pc.normalise(pytest_mark("xfail", reason="function"))
    farther = pc.normalise(pytest_mark("xfail", reason="module", strict=True))
    assert pc.fold([closest, farther], "passed", "") == ("xpass", "function")
    skip = pc.normalise(pytest_mark("skip"))
    assert pc.fold([skip], "passed", "", runtime=True) == ("skipped", "skipped at runtime")
    assert pc.fold([pc.normalise(pytest_mark("skip", reason="why"))], "failed", "d", runtime=True) == ("skipped", "why")
    assert pc.fold([pc.normalise(pytest_mark("slow"))], "failed", "d") == ("failed", "d")


def test_the_marker_api_iterates_closest_first_on_both_bearers():
    class Node(pc.MarkerBearer):
        def __init__(self):
            self.own_markers = []

    n = Node()
    n.add_marker(pytest_mark("xfail", reason="first"))
    n.add_marker(pytest_mark("skip", reason="second"))
    n.add_marker(pytest_mark("xfail", reason="third"))
    assert [m.kwargs["reason"] for m in n.iter_markers()] == ["third", "second", "first"]
    assert n.get_closest_marker("xfail").kwargs["reason"] == "third"
    assert n.get_closest_marker("nope", "dflt") == "dflt"
    n.add_marker(pytest_mark("skip", reason="farthest"), append=False)
    assert [m.kwargs["reason"] for m in n.iter_markers("skip")] == ["second", "farthest"]
