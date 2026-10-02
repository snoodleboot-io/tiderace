"""The tier decided once (TID-123, step 4): every routing rule as a row, and the response assembled
from the variants' results in the shape the engine reads."""
from __future__ import annotations

import os
import sys

import pytest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(HERE))

from tiderace_shim import results, tiers  # noqa: E402
from tiderace_shim.pytest_compat import normalise_all  # noqa: E402
from tiderace_shim.tiers import Routing, Tier, VariantResult, route  # noqa: E402


def routing(**overrides):
    base = dict(fork_available=True, in_module_child=False, module_child_holds_module=False,
                no_fork=False, restore=True, force_no_fork=False, trusted_pure=False,
                recorded_must_fork=False)
    base.update(overrides)
    return Routing(**base)


RESTORABLE = lambda: True  # noqa: E731
OPAQUE = lambda: False  # noqa: E731


def boom():
    raise RuntimeError("cannot import")


@pytest.mark.parametrize("inputs, restorable, tier", [
    # the ladder's default: the engine asks for in-process, the module restores → restore tier
    (routing(force_no_fork=True), RESTORABLE, Tier.RESTORE),
    # recorded pure and unchanged: bare, and restorability is not even asked
    (routing(force_no_fork=True, trusted_pure=True), boom, Tier.BARE),
    # nothing asked for in-process: a fork per variant
    (routing(), boom, Tier.FORK),
    # an opaque module under the ladder: the module's own child
    (routing(force_no_fork=True), OPAQUE, Tier.MODULE_CHILD),
    # …and one that cannot even be inspected: be safe, fork
    (routing(force_no_fork=True), boom, Tier.MODULE_CHILD),
    # `--no-fork` with restore: the same gate applies to whole-run no-fork
    (routing(no_fork=True), OPAQUE, Tier.MODULE_CHILD),
    (routing(no_fork=True), RESTORABLE, Tier.RESTORE),
    # `--no-fork` without restore: in-process, nothing to snapshot
    (routing(no_fork=True, restore=False), boom, Tier.IN_PROCESS),
    # a recorded disturber under the ladder takes the module child (TID-96)
    (routing(force_no_fork=True, recorded_must_fork=True), RESTORABLE, Tier.MODULE_CHILD),
    # …but not under `--no-fork`, where the restore is the whole remedy
    (routing(no_fork=True, recorded_must_fork=True), RESTORABLE, Tier.RESTORE),
    # a module child already open for this file takes the rest of it
    (routing(module_child_holds_module=True), boom, Tier.MODULE_CHILD),
    # inside the module child nothing forks again: its tests run in-process
    (routing(in_module_child=True, force_no_fork=True, restore=False), boom, Tier.IN_PROCESS),
    # Windows: the module needs a fork and there is none — reported, not run
    (routing(force_no_fork=True, fork_available=False), OPAQUE, Tier.REFUSED),
    (routing(no_fork=True, fork_available=False), RESTORABLE, Tier.RESTORE),
])
def test_the_tier_is_decided_from_its_inputs(inputs, restorable, tier):
    assert route(inputs, restorable) is tier


def test_in_process_is_the_three_tiers_that_run_here():
    assert [t.in_process for t in Tier] == [True, True, True, False, False, False]


def keywords(nid):
    return [nid]


def test_an_unparametrized_node_assembles_the_old_frame():
    r = [VariantResult("t.py::a", "passed", "", {"src.py": {1, 2}}, None, False, 3)]
    assert tiers.assemble("t.py::a", r, parametrized=False, native_marks=[], pytest_marks=[], keywords=keywords) == {
        "node_id": "t.py::a", "outcome": "passed", "detail": "", "keywords": ["t.py::a"],
        "coverage": {"src.py": [1, 2]}, "pure": True}


def test_a_parametrized_node_carries_its_variants_and_the_worst_outcome():
    r = [VariantResult("t.py::a[1]", "passed", "", {}, None, False, 1),
         VariantResult("t.py::a[2]", "failed", "assert", {}, "mutated module global `X`", True, 2),
         VariantResult("t.py::a[3]", "skipped", "s", {}, results.UNKNOWN_PURITY, False, 0)]
    resp = tiers.assemble("t.py::a", r, parametrized=True, native_marks=[], pytest_marks=[], keywords=keywords)
    assert (resp["outcome"], resp["detail"]) == ("failed", "assert")
    assert [v["node_id"] for v in resp["variants"]] == ["t.py::a[1]", "t.py::a[2]", "t.py::a[3]"]
    assert resp["variants"][0] == {"node_id": "t.py::a[1]", "outcome": "passed", "detail": "", "duration_ms": 1,
                                   "keywords": ["t.py::a[1]"], "pure": True}
    assert resp["variants"][1]["pure"] is False and resp["variants"][1]["must_fork"] is True
    assert "pure" not in resp["variants"][2]
    assert resp["pure"] is False and resp["impurity"] == "mutated module global `X`" and resp["must_fork"] is True
    assert "coverage" not in resp


def test_marks_fold_native_first_then_pytests():
    from types import SimpleNamespace
    native = normalise_all([SimpleNamespace(kind="xfail", reason="native", condition=True, strict=False, name="")])
    pyt = normalise_all([SimpleNamespace(name="xfail", args=(), kwargs={"reason": "pytest"})])
    r = [VariantResult("t.py::a", "failed", "d", {}, results.UNKNOWN_PURITY, False, 1)]
    resp = tiers.assemble("t.py::a", r, parametrized=False, native_marks=native, pytest_marks=pyt, keywords=keywords)
    assert (resp["outcome"], resp["detail"]) == ("xfail", "native")
    assert "pure" not in resp  # nothing measured: no verdict
