"""What discovery produced, as one object (TID-124, step 2): the lookups the gate makes, and a real
discovery over a small suite — conftest options, ini declarations, a collection hook's skips, a
directory a conftest skipped."""
from __future__ import annotations

import importlib
import os
import sys
import textwrap
import types

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(HERE))

from tiderace_shim import _shim, discovery  # noqa: E402
from tiderace_shim.config import RunConfig  # noqa: E402
from tiderace_shim.discovery import Discovery, dir_mark  # noqa: E402


def test_a_directory_mark_covers_its_subtree_from_the_nearest_ancestor():
    marks = {"tests": "slow", "tests/unit": "unit-only"}
    assert dir_mark(marks, "tests/unit/test_a.py") == "unit-only"
    assert dir_mark(marks, "tests/int/test_b.py") == "slow"
    assert dir_mark(marks, "other/test_c.py") is None
    assert dir_mark({"": "everything"}, "anything/at/all.py") == "everything"
    assert dir_mark({}, "x.py") is None
    disc = Discovery(None, dir_skips={"sub": "no backend"}, dir_errors={"": "broken"})
    assert disc.dir_skip("sub/test_x.py") == "no backend" and disc.dir_skip("test_y.py") is None
    assert disc.dir_error("sub/test_x.py") == "broken"


def test_governing_conftests_are_deepest_first_with_ancestors_last():
    root, deep, side, up = (types.ModuleType(n) for n in ("c_root", "c_deep", "c_side", "c_up"))
    disc = Discovery(None, conftest_scopes=[("..", up), ("", root), ("tests/unit", deep), ("tests/other", side)])
    assert disc.conftests_governing("tests/unit/test_a.py") == [deep, root, up]
    assert disc.conftests_governing("tests/test_b.py") == [root, up]
    assert not disc.generate_tests_hooks
    deep.pytest_generate_tests = lambda metafunc: None
    assert disc.generate_tests_hooks


def test_a_real_discovery_records_what_the_gate_and_the_config_read(tmp_path, monkeypatch):
    (tmp_path / "pytest.ini").write_text("[pytest]\n")
    (tmp_path / "conftest.py").write_text(textwrap.dedent("""
        import pytest

        def pytest_addoption(parser):
            parser.addoption("--real", action="store_true", help="hit the real backend")
            parser.addini("db_url", "where the database is", default="sqlite://")

        def pytest_collection_modifyitems(config, items):
            for item in items:
                if "needs_backend" in item.keywords and not config.getoption("--real"):
                    item.add_marker(pytest.mark.skip(reason="pass --real"))

        @pytest.fixture
        def widget():
            return 1
    """))
    (tmp_path / "test_disc_a.py").write_text(textwrap.dedent("""
        import pytest

        @pytest.mark.needs_backend
        def test_backend(widget):
            pass

        def test_plain():
            pass
    """))
    (tmp_path / "sub").mkdir()
    (tmp_path / "sub" / "conftest.py").write_text("import pytest\npytest.importorskip('tiderace_no_such_module_xyz')\n")
    (tmp_path / "sub" / "test_disc_b.py").write_text("def test_never():\n    pass\n")
    root = str(tmp_path)
    monkeypatch.syspath_prepend(root)
    importlib.invalidate_caches()
    try:
        run = RunConfig.load(root)
        disc = _shim._discover(run)
    finally:
        for name in ("test_disc_a", "test_disc_b", "_fx_conftest_root", "_fx_conftest_sub"):
            sys.modules.pop(name, None)
    assert disc.cli_options["real"] is False  # (plus whatever installed plugins declare, TID-87)
    assert disc.ini_declared["db_url"] == (None, "sqlite://")
    assert disc.marker_skips == {"test_disc_a.py::test_backend": "pass --real"}
    assert disc.dir_skip("sub/test_disc_b.py") is not None and disc.dir_skip("test_disc_a.py") is None
    assert [loc for loc, _ in disc.conftest_scopes] == [""]  # sub's conftest skipped itself: not a scope
    assert disc.ancestors == [] and disc.dir_errors == {} and disc.hook_marks == {}
    assert "widget" in disc.registry.by_name
    config = _shim._Config(run, disc)
    assert config.getoption("--real") is False and config.getini("db_url") == "sqlite://"
    assert config.getini("nobody_declared") is None
    engine = _shim.Engine(disc, run, no_fork=True)
    assert engine.reg is disc.registry and engine.discovery is disc
