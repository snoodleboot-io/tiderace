"""What a worker process holds (TID-124, step 3): its state and its memos as objects on the
engine, the in-process deadline carrying its own message, and the node resolver taking the run
root as a parameter — its name derivation pure, its import the one place `sys.path` grows."""
from __future__ import annotations

import os
import sys
import types

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(HERE))

from tiderace_shim import footprint, invoke, nodes, tiers  # noqa: E402


def test_the_current_node_is_one_object_for_the_whole_test():
    state = invoke.ProcessState()
    assert state.current_node is None and state.nodes_run == 0 and state.clean_room is None
    node = state.node_for("t.py::test_a")
    assert node.nodeid == "t.py::test_a" and node.function is None
    assert state.node_for("t.py::test_a", func=len) is node and node.function is len  # the fixture's, then the test's
    other = state.node_for("t.py::test_b")
    assert other is not node and state.current_node is other


def test_xunit_bookkeeping_is_per_state():
    a, b = invoke.ProcessState(), invoke.ProcessState()
    calls = []

    Mod = types.ModuleType("m")
    Mod.setup_module = lambda module: calls.append(("setup", module))
    Mod.teardown_module = lambda module: calls.append(("teardown", module))
    invoke._xunit_module_setup(a.xunit, Mod)
    invoke._xunit_module_setup(a.xunit, Mod)  # once per process
    assert calls == [("setup", Mod)] and ("module", "m") in a.xunit.done and not b.xunit.done
    sys.modules["m"] = Mod
    try:
        invoke._xunit_module_teardown(a.xunit)
    finally:
        sys.modules.pop("m", None)
    assert calls[-1] == ("teardown", Mod) and not a.xunit.done


def test_the_deadline_carries_its_message():
    deadline = tiers._in_process_deadline(1500)
    assert deadline.message.startswith("timeout after 1.5s on the in-process tier")
    assert str(tiers._InProcessTimeout()) == "timeout on the in-process tier"  # the watchdog's bare class
    assert str(tiers._InProcessTimeout(deadline.message)) == deadline.message


def test_caches_are_fresh_per_engine_and_memoise(tmp_path):
    caches = footprint.Caches()
    assert caches.file_deps_stats == {"hits": 0, "parsed": 0} and caches.resolved == {}
    (tmp_path / "pkg").mkdir()
    (tmp_path / "pkg" / "__init__.py").write_text("")
    (tmp_path / "pkg" / "util.py").write_text("x = 1\n")
    (tmp_path / "test_c.py").write_text("from pkg import util\n")
    root = str(tmp_path)
    sys.path.insert(0, root)
    try:
        closure = footprint._import_closure(caches, "test_c.py", root)
        assert closure == frozenset({"pkg/__init__.py", "pkg/util.py"})
        assert caches.import_closure["test_c.py"] is closure and caches.file_deps_stats["parsed"] >= 1
        assert footprint._import_closure(footprint.Caches(), "test_c.py", root) == closure  # a fresh memo: recomputed
    finally:
        sys.path.remove(root)


def test_the_module_name_is_pure_and_the_import_grows_sys_path(tmp_path, monkeypatch):
    (tmp_path / "tests").mkdir()
    (tmp_path / "tests" / "test_pure_name.py").write_text("VALUE = 7\n")
    root = str(tmp_path)
    nodes._module_name_walk.cache_clear()
    before = list(sys.path)
    assert nodes.module_name("tests/test_pure_name.py", root) == "test_pure_name"
    assert sys.path == before  # deriving the name touched nothing
    try:
        module = nodes.import_module("tests/test_pure_name.py", root)
        assert module.VALUE == 7 and sys.path[0] == str(tmp_path / "tests")
    finally:
        sys.modules.pop("test_pure_name", None)
        if str(tmp_path / "tests") in sys.path:
            sys.path.remove(str(tmp_path / "tests"))
