"""The run's configuration and its selection as objects (TID-124, step 1): what the engine used to
read from module globals, loaded from a project and patched per run."""
from __future__ import annotations

import os
import sys
import textwrap

import pytest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(HERE))

from tiderace_shim import selection  # noqa: E402
from tiderace_shim.config import RunConfig, load_project_config  # noqa: E402
from tiderace_shim.selection import Selection  # noqa: E402


def project(tmp_path, ini: str = "", layout: dict | None = None):
    (tmp_path / "pytest.ini").write_text("[pytest]\n" + textwrap.dedent(ini))
    for rel, text in (layout or {}).items():
        path = tmp_path / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)
    return load_project_config(str(tmp_path))


# ------------------------------------------------------------------------------ Selection
def test_a_selection_is_loaded_from_the_project_and_the_command_line_wins(tmp_path):
    proj = project(tmp_path, 'addopts = -m "not slow" -k unit --strict-markers\nmarkers =\n    slow: takes a while\n    db(x): needs a database\n')
    sel = Selection.load(proj, env={})
    assert sel.strict_markers and sel.declared_marks == {"slow", "db"}
    assert sel.marker_allows({"fast"}) and not sel.marker_allows({"slow"})
    assert sel.keyword_verdict(["tests", "unit", "test_a.py", "test_x"], final=False) is True
    assert sel.keyword_verdict(["tests", "test_a.py", "test_x"], final=True) is False
    assert sel.keyword_verdict(["tests", "test_a.py", "test_x"], final=False) is None  # a case id may still match
    env = {"TIDERACE_MARKER_EXPR": "slow", "TIDERACE_KEYWORD_EXPR": "x"}
    sel = Selection.load(proj, env=env)
    assert sel.marker_allows({"slow"}) and not sel.marker_allows({"fast"})
    assert sel.keyword_verdict(["test_x"], final=True) is True


def test_no_selection_selects_everything(tmp_path):
    sel = Selection.load(project(tmp_path), env={})
    assert sel == Selection()
    assert sel.keyword_verdict(["anything"], final=True) is True
    assert sel.marker_allows(set()) and sel.unknown_marks({"typo"}, str(tmp_path)) == []


def test_a_malformed_expression_is_ignored_and_reported_once(tmp_path, capsys):
    sel = Selection.load(project(tmp_path, 'addopts = -k "(unit"'), env={})
    assert sel.keyword is None
    assert "ignoring -k '(unit'" in capsys.readouterr().err


def test_override_keeps_absent_or_null_fields_and_clears_on_an_empty_string(tmp_path):
    base = Selection.load(project(tmp_path, 'addopts = -m "not slow"'), env={})
    assert base.override(None) is base and base.override({}) is base
    same = base.override({"keyword": None, "marker": None, "strict_markers": False})
    assert same.marker is base.marker and same.keyword is None and not same.strict_markers  # TID-102
    patched = base.override({"keyword": "x and not y", "strict_markers": True})
    assert patched.marker is base.marker and patched.strict_markers
    assert patched.keyword_verdict(["x"], final=True) is True
    assert patched.keyword_verdict(["x", "y"], final=True) is False
    cleared = patched.override({"marker": ""})
    assert cleared.marker is None and cleared.keyword is patched.keyword
    assert base.marker is not None  # the image's own selection is untouched: a new object each time


def test_unknown_marks_under_strict_checking(tmp_path, monkeypatch):
    sel = Selection(strict_markers=True, declared_marks=frozenset({"slow"}))
    monkeypatch.setattr(selection, "plugin_marks", lambda root: frozenset({"timeout"}))
    assert sel.unknown_marks({"slow", "skip", "timeout", "slwo"}, "/r") == ["slwo"]
    monkeypatch.setattr(selection, "plugin_marks", lambda root: None)  # pytest could not say: enforce nothing
    assert sel.unknown_marks({"slwo"}, "/r") == []


def test_the_grammar_is_three_valued():
    tree = selection.parse_expr("(a or b) and not c")
    assert tree == ("and", [("or", [("ident", "a"), ("ident", "b")]), ("not", ("ident", "c"))])
    assert selection.evaluate(tree, {"a": True, "c": False}.get) is True
    assert selection.evaluate(tree, {"a": True, "c": None}.get) is None
    assert selection.evaluate(tree, {"a": None, "b": None, "c": True}.get) is False
    with pytest.raises(ValueError):
        selection.parse_expr("a and")
    assert selection.parse_expr("test_x[1-a]") == ("ident", "test_x[1-a]")


# ------------------------------------------------------------------------------ RunConfig
def test_a_run_config_ignores_what_the_project_ignores_and_selects_the_modules_file(tmp_path):
    proj_dir = tmp_path
    project(proj_dir, "addopts = --ignore=tests/perf --ignore-glob=*_slow.py", {"tests/perf/test_p.py": "", "tests/test_a.py": ""})
    modules = tmp_path / "modules.txt"
    modules.write_text("tests/test_a.py\n\n")
    run = RunConfig.load(str(proj_dir), modules_file=str(modules))
    assert run.project.dir == str(proj_dir) and run.modules == {"tests/test_a.py"}
    assert run.is_ignored(str(proj_dir / "tests" / "perf")) and run.module_ignored("tests/perf/test_p.py")
    assert run.is_ignored(str(proj_dir / "tests" / "test_slow.py")) and not run.module_ignored("tests/test_a.py")
    assert run.module_selected("tests/test_a.py") and not run.module_selected("tests/test_b.py")
    assert RunConfig.load(str(proj_dir)).module_selected("tests/test_b.py")  # no file: every module
    assert not run.force_asyncio


def test_keyword_names_are_the_path_below_the_rootdir_then_the_segments_then_the_marks(tmp_path):
    project(tmp_path, "", {"tests/unit/test_a.py": ""})
    run = RunConfig.load(str(tmp_path))
    names = run.keyword_names("tests/unit/test_a.py::TestC::test_x[1-a]", {"slow", "db"})
    if selection.pytest_major() >= 8:
        assert names == ["tests", "unit", "test_a.py", "TestC", "test_x[1-a]", "db", "slow"]
    else:
        assert names == ["tests/unit/test_a.py", "TestC", "test_x[1-a]", "db", "slow"]
    # The run root below the rootdir: the names still start at the rootdir (TID-100).
    below = RunConfig.load(str(tmp_path / "tests"))
    assert below.project.dir == str(tmp_path)
    assert below.keyword_names("unit/test_a.py::t", set())[:1] == (["tests"] if selection.pytest_major() >= 8 else ["tests/unit/test_a.py"])
