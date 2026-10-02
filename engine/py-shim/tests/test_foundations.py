"""The shim's foundations (TID-121): the wire shapes `results.py` spells, the target resolver, and
the project config loader. Run with the fx venv:

    .tiderace-fx-venv/bin/python -m pytest engine/py-shim/tests -q

The result shapes are pinned to the literals the shim used to write by hand — the Rust side reads
exactly these — so a change here is a change to the wire.
"""
from __future__ import annotations

import os
import sys
import textwrap

import pytest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(HERE))  # the package beside `tests/`

from tiderace_shim import config, nodes, results  # noqa: E402


# ------------------------------------------------------------------------------ results
def test_the_shapes_are_the_old_literals():
    assert results.empty_expansion("t.py::a") == {
        "node_id": "t.py::a", "outcome": "passed", "expanded": True, "variants": []}
    assert results.empty_expansion("t.py::a", keywords=["t.py", "a"]) == {
        "node_id": "t.py::a", "outcome": "passed", "expanded": True, "variants": [],
        "keywords": ["t.py", "a"]}
    assert results.errored("t.py::a", "boom") == {"node_id": "t.py::a", "outcome": "error", "detail": "boom"}
    assert results.skipped("t.py::a", "no backend", skip_origin="t.py") == {
        "node_id": "t.py::a", "outcome": "skipped", "detail": "no backend", "skip_origin": "t.py"}
    assert results.variant("t.py::a[1]", "passed", "", 3, keywords=["a"]) == {
        "node_id": "t.py::a[1]", "outcome": "passed", "detail": "", "duration_ms": 3, "keywords": ["a"]}
    assert results.expansion("t.py::C", "failed", "x", [{"node_id": "t.py::C::t"}]) == {
        "node_id": "t.py::C", "outcome": "failed", "detail": "x", "expanded": True,
        "variants": [{"node_id": "t.py::C::t"}]}


def test_outcomes_serialise_as_their_text_and_compare_to_it():
    import json
    assert json.dumps({"outcome": results.Outcome.ERROR}) == '{"outcome": "error"}'
    assert results.Outcome.PASSED == "passed"
    assert results.errored("n", "d")["outcome"] == "error"


def test_worst_is_the_one_ordering():
    worst = results.Outcome.worst
    assert worst([("passed", ""), ("skipped", "s"), ("failed", "f"), ("error", "e")]) == ("error", "e")
    assert worst([("passed", ""), ("failed", "f"), ("skipped", "s")]) == ("failed", "f")
    assert worst([("passed", ""), ("skipped", "s")]) == ("skipped", "s")
    assert worst([("xfail", "x"), ("passed", "")]) == ("xfail", "x")  # equal rank: the first wins, as before


def test_purity_round_trips_the_tri_state():
    assert results.with_purity({}, None) == {"pure": True}
    assert results.with_purity({}, "touched sys.modules", reason_key="impurity") == {
        "pure": False, "impurity": "touched sys.modules"}
    assert results.with_purity({}, "impure") == {"pure": False}
    assert results.with_purity({}, results.UNKNOWN_PURITY) == {}
    assert results.purity_from({}) is results.UNKNOWN_PURITY
    assert results.purity_from({"pure": True}) is None
    assert results.purity_from({"pure": False}) == "impure"
    assert results.purity_from({"pure": False, "impurity": "why"}) == "why"


# ------------------------------------------------------------------------------ nodes
def test_a_target_resolves_the_chain(tmp_path, monkeypatch):
    (tmp_path / "test_t.py").write_text(textwrap.dedent('''
        import unittest

        def test_plain():
            pass

        async def test_coro():
            pass

        class TestK:
            def test_m(self):
                pass

        class TestU(unittest.TestCase):
            async def test_async(self):
                pass
    '''))
    monkeypatch.chdir(tmp_path)
    root = str(tmp_path)
    nodes._module_name_walk.cache_clear()
    t = nodes.resolve_target("test_t.py::test_plain", "function", root)
    assert (t.module_key, t.name, t.cls, t.is_unittest, t.is_async) == ("test_t.py", "test_plain", None, False, False)
    assert t.owners == (t.module, t.func)
    assert nodes.resolve_target("test_t.py::test_coro", "function", root).is_async
    k = nodes.resolve_target("test_t.py::TestK::test_m", "class_method", root)
    assert k.cls.__name__ == "TestK" and k.name == "test_m" and not k.is_unittest
    assert k.owners == (k.module, k.cls, k.func)
    # A unittest class the collector labelled as a pytest class: the live class says otherwise,
    # and its coroutines are its own to drive (TID-51).
    u = nodes.resolve_target("test_t.py::TestU::test_async", "class_method", root)
    assert u.is_unittest and not u.is_async
    with pytest.raises(AttributeError):
        nodes.resolve_target("test_t.py::TestK::nope", "class_method", root)
    lenient = nodes.resolve_target("test_t.py::Gone::nope", "class_method", root, lenient=True)
    assert lenient.cls is None and lenient.func is None and lenient.owners == (lenient.module,)


def test_module_naming_roots_at_the_first_non_package(tmp_path):
    pkg = tmp_path / "tests" / "unit"
    pkg.mkdir(parents=True)
    (tmp_path / "tests" / "__init__.py").write_text("")
    (pkg / "__init__.py").write_text("")
    nodes._module_name_walk.cache_clear()
    assert nodes._module_name_walk("tests/unit/test_x.py", str(tmp_path)) == (str(tmp_path), "tests.unit.test_x")
    assert nodes._module_name_walk("tests/other/test_y.py", str(tmp_path)) == (
        str(tmp_path / "tests" / "other"), "test_y")
    assert nodes.module_key("tests/m.py::C::t") == "tests/m.py"
    assert nodes.class_method("tests/m.py::C::t") == ("C", "t")


# ------------------------------------------------------------------------------ config
def test_option_values_in_the_spellings_pytest_accepts():
    argv = ["-m", "not slow", "-k=unit", "-pno:cacheprovider", "-p", "xdist", "--ignore=tests/perf",
            "--ignore", "tests/slow", "--strict-markers", "-mfast"]
    assert config.option_values(argv, "-m") == ["not slow", "fast"]
    assert config.option_values(argv, "-k") == ["unit"]
    assert config.option_values(argv, "-p") == ["no:cacheprovider", "xdist"]
    assert config.option_values(argv, "--ignore") == ["tests/perf", "tests/slow"]
    assert "--strict-markers" in argv  # a bare flag is asked for with `ProjectConfig.flag`, not `option`
    assert config.option(["--pool", "4", "--modules=m.txt"], "--pool") == "4"
    assert config.option(["--modules=m.txt"], "--modules") == "m.txt"


def test_the_nearest_config_with_a_pytest_section_is_the_rootdir(tmp_path):
    (tmp_path / "pyproject.toml").write_text('[tool.pytest.ini_options]\naddopts = "-m \\"not slow\\" --ignore=tests/perf"\nmarkers = ["slow: slow", "db"]\n[tool.tiderace]\nplugins = []\n')
    sub = tmp_path / "pkg"
    sub.mkdir()
    (sub / "pyproject.toml").write_text('[tool.black]\nline-length = 100\n')  # no pytest section: not the config
    project = config.load_project_config(str(sub))
    assert project.dir == str(tmp_path) and project.source == str(tmp_path / "pyproject.toml")
    assert project.argv == ("-m", "not slow", "--ignore=tests/perf")
    assert project.opt("-m") == "not slow" and project.opt("-k") is None
    assert project.opt_values("--ignore") == ["tests/perf"]
    assert project.values("markers") == ["slow: slow", "db"]
    assert project.setting("plugins") == [] and project.setting("nope") is config.NOTSET
    assert not project.flag("--strict-markers")


def test_an_empty_pytest_ini_counts_and_ini_values_split_on_lines(tmp_path):
    (tmp_path / "pytest.ini").write_text("[pytest]\n")
    (tmp_path / "setup.cfg").write_text("[tool:pytest]\naddopts = --strict-markers\nmarkers =\n    a\n    b: desc\n")
    project = config.load_project_config(str(tmp_path))
    assert project.source == str(tmp_path / "pytest.ini")  # pytest.ini outranks setup.cfg
    assert project.addopts == ""  # the chosen file's addopts, not setup.cfg's
    assert project.values("markers") == ["a", "b: desc"]  # every file in the dir is read for values
    assert project.values("addopts") == ["--strict-markers"]


def test_pytest_9s_native_table_and_no_config_at_all(tmp_path, tmp_path_factory):
    (tmp_path / "pyproject.toml").write_text('[tool.pytest]\naddopts = ["-k", "unit"]\n')
    project = config.load_project_config(str(tmp_path))
    assert project.addopts == "-k unit" and project.opt("-k") == "unit"
    bare = tmp_path_factory.mktemp("bare")  # beside `tmp_path`, not under it: nothing above carries a section
    none = config.load_project_config(str(bare))
    assert none.source is None and none.dir == str(bare) and none.argv == () and none.values("markers") == []
