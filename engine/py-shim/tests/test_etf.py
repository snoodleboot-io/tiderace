"""Estuary Trace Format output (TID-128): the trace plan, and the file written from it.

    .tiderace-fx-venv/bin/python -m pytest engine/py-shim/tests/test_etf.py -q

`etf.build` names what each test read and factors the names into shared fragments. It needs no
binding, so everything about *what a trace holds* is tested here on any interpreter. The tests
that write and decode a file need `estuary_etf` — an optional dependency — and skip without it.
"""
from __future__ import annotations

import os
import sys

import pytest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(HERE))  # the package beside `tests/`

from tiderace_shim import etf, footprint  # noqa: E402


def _tree(tmp_path, files: dict) -> str:
    for rel, body in files.items():
        path = tmp_path / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(body)
    return str(tmp_path)


def _run(root: str, run_root: str | None = None, **kw) -> etf.Run:
    return etf.Run(os.path.abspath(run_root or root), os.path.abspath(root), "1.2.3", **kw)


def _test(check: str, files=(), outcome: str = "passed") -> etf.Test:
    return etf.Test(check, check.split("::", 1)[0], outcome, 2, {"file": list(files)})


def _files(keys) -> set:
    return {name for kind, name in keys if kind == "file"}


@pytest.fixture
def project(tmp_path, monkeypatch):
    """Two test modules importing one package, a root conftest that imports a helper, a nested
    conftest, and an import cycle."""
    root = _tree(tmp_path, {
        "pytest.ini": "[pytest]\naddopts = -q\n",
        "pyproject.toml": "[project]\nname = 'p'\n\n[tool.tiderace]\nx = 1\n",
        "conftest.py": "import helper\n",
        "helper.py": "H = 1\n",
        "pkg/__init__.py": "",
        "pkg/core.py": "from pkg import util\n",
        "pkg/util.py": "from pkg import core\n",  # a cycle: core <-> util
        "pkg/leaf.py": "X = 1\n",
        "tests/unit/conftest.py": "import pkg.leaf\n",
        "tests/unit/test_a.py": "from pkg import core\n",
        "tests/unit/test_b.py": "from pkg import core\nimport pkg.leaf\n",
        "tests/other/test_c.py": "import os\n",
    })
    monkeypatch.syspath_prepend(root)
    return root


# ------------------------------------------------------------------------------ the plan
def test_every_test_of_a_module_reads_the_modules_import_closure(project):
    """TID-133: the closure belongs to the module, so to every test in it — whatever each one's
    own footprint happens to hold."""
    run = _run(project)
    first = _test("tests/unit/test_a.py::test_first", ["tests/unit/test_a.py", "pkg/core.py"])
    later = [_test(f"tests/unit/test_a.py::test_{i}", ["tests/unit/test_a.py"]) for i in range(19)]
    plan = etf.build(run, [first, *later])
    assert len(plan.records) == 20
    for record in plan.records:
        files = _files(plan.trace(record))
        assert {"tests/unit/test_a.py", "pkg/core.py", "pkg/util.py", "pkg/__init__.py"} <= files, record.check
        assert "pkg/leaf.py" in files  # through tests/unit/conftest.py
    # Twenty records, one root: nothing in any test's own footprint was outside the shared fragments.
    assert len({r.root for r in plan.records}) == 1
    assert plan.fragments[plan.records[0].root].keys == frozenset()


def test_the_import_closure_is_shared_between_modules_and_a_cycle_is_one_fragment(project):
    run = _run(project)
    plan = etf.build(run, [_test("tests/unit/test_a.py::t"), _test("tests/unit/test_b.py::t")])
    memo = plan.memo["imports"]
    core, util = (memo[os.path.join(project, "pkg", name)] for name in ("core.py", "util.py"))
    assert core == util, "an import cycle cannot be two fragments that include each other"
    assert _files(plan.fragments[core].keys) == {"pkg/core.py", "pkg/util.py"}
    a = plan.fragments[memo[os.path.join(project, "tests", "unit", "test_a.py")]]
    b = plan.fragments[memo[os.path.join(project, "tests", "unit", "test_b.py")]]
    assert core in a.children and core in b.children, "both modules point at the one fragment"
    assert _files(a.keys) == {"tests/unit/test_a.py"}
    # Children are defined before the fragments that include them: what the writer needs.
    for index, fragment in enumerate(plan.fragments):
        assert all(child < index for child in fragment.children)


def test_the_conftest_chain_runs_from_the_root_to_the_tests_directory(project):
    """TID-132: each conftest with what it imports; a directory without one still names the key."""
    run = _run(project)
    plan = etf.build(run, [_test("tests/unit/test_a.py::t"), _test("tests/other/test_c.py::t")])
    unit, other = (_files(plan.trace(r)) for r in sorted(plan.records, key=lambda r: r.check, reverse=True))
    assert {"conftest.py", "helper.py", "tests/conftest.py", "tests/unit/conftest.py", "pkg/leaf.py"} <= unit
    assert {"conftest.py", "helper.py", "tests/conftest.py", "tests/other/conftest.py"} <= other
    assert "tests/unit/conftest.py" not in other and "pkg/leaf.py" not in other
    chain = plan.memo["conftests"]
    assert set(chain) == {"", "tests", "tests/unit", "tests/other"}
    assert chain["tests"] in plan.fragments[chain["tests/unit"]].children
    assert chain["tests"] in plan.fragments[chain["tests/other"]].children
    assert plan.fragments[chain["tests"]].keys == {("file", "tests/conftest.py")}  # absent: named anyway


def test_config_is_one_key_per_section_the_run_reads(project):
    run = _run(project)
    plan = etf.build(run, [_test("tests/other/test_c.py::t")])
    keys = plan.trace(plan.records[0])
    derived = {name for kind, name in keys if kind == "derived"}
    assert derived == {"cfg/pytest.ini#pytest", "cfg/pyproject.toml#tool.pytest.ini_options",
                       "cfg/pyproject.toml#tool.pytest", "cfg/pyproject.toml#tool.tiderace"}
    assert {"tox.ini", "setup.cfg"} <= _files(keys), "a config file that could exist and does not"
    assert "pyproject.toml" not in _files(keys), "its sections are the keys, not the file"
    assert plan.inputs[("derived", "cfg/pytest.ini#pytest")] == b'{"addopts":"-q"}'
    assert plan.inputs[("derived", "cfg/pyproject.toml#tool.tiderace")] == b'{"x":1}'
    assert plan.inputs[("derived", "cfg/pyproject.toml#tool.pytest")] == b"{}"


def test_the_toolchain_is_one_fragment_under_every_root(project):
    run = _run(project)
    plan = etf.build(run, [_test("tests/unit/test_a.py::t"), _test("tests/other/test_c.py::t")])
    assert plan.fragments[plan.toolchain].keys == {("tool", "python"), ("tool", "pyenv"), ("tool", "tiderace")}
    assert all(plan.toolchain in plan.fragments[r.root].children for r in plan.records)
    assert plan.inputs[("tool", "tiderace")] == b"1.2.3"
    assert sys.implementation.name.encode() in plan.inputs[("tool", "python")]


def test_a_tests_own_pairs_are_what_the_shared_fragments_do_not_hold(project):
    run = _run(project)
    plan = etf.build(run, [_test("tests/other/test_c.py::t", ["tests/other/test_c.py", "pkg/leaf.py", "helper.py"])])
    root = plan.fragments[plan.records[0].root]
    assert root.keys == {("file", "pkg/leaf.py")}, "executed, imported by nothing it reads through"


def test_outcomes_map_onto_etfs_four_and_records_come_in_check_order(project):
    run = _run(project)
    tests = [_test(f"tests/other/test_c.py::t_{o}", outcome=o)
             for o in ("xpass", "passed", "failed", "error", "skipped", "xfail", "what")]
    plan = etf.build(run, tests)
    assert [r.check for r in plan.records] == sorted(t.check for t in tests)
    by = {r.check.rsplit("_", 1)[1]: r.outcome for r in plan.records}
    assert by == {"passed": "pass", "failed": "fail", "error": "error", "skipped": "skip",
                  "xfail": "pass", "xpass": "pass", "what": "error"}
    assert plan.records[0].duration_us == 2000


def test_ids_and_keys_are_repository_relative_wherever_the_run_started(project):
    """`tiderace run tests/` reports `unit/test_a.py::t`; the check id and the keys are still
    the repository's, so the same test is the same check from either root."""
    manifest = {"tests": [{"id": "unit/test_a.py::t[1-2]", "outcome": "passed", "duration_ms": 1,
                           "reads": {"file": ["tests/unit/test_a.py", "../outside.py", "/abs.py", "a//b.py",
                                              "venv/lib/x.py"]}}]}
    run = _run(project, run_root=os.path.join(project, "tests"),
               toolchain_dirs=(os.path.join(project, "venv"),))
    (test,) = etf.tests_from(manifest, run)
    assert test.check == "tests/unit/test_a.py::t[1-2]" and test.module == "tests/unit/test_a.py"
    assert test.reads == {"file": ["tests/unit/test_a.py"]}, "canonical repository paths only"
    same = etf.tests_from({"tests": [{"id": "tests/unit/test_a.py::t[1-2]"}]}, _run(project))
    assert same[0].check == test.check


def test_a_virtualenv_in_the_checkout_is_the_toolchains_not_a_set_of_file_keys(project, tmp_path):
    site = tmp_path / "venv" / "lib" / "site-packages" / "dep"
    site.mkdir(parents=True)
    (site / "__init__.py").write_text("")
    (tmp_path / "tests" / "other" / "test_c.py").write_text("import dep\n")
    sys.path.insert(0, str(site.parent))
    try:
        keyed = etf.build(_run(project), [_test("tests/other/test_c.py::t")])
        skipped = etf.build(_run(project, toolchain_dirs=(str(tmp_path / "venv"),)),
                            [_test("tests/other/test_c.py::t")])
    finally:
        sys.path.remove(str(site.parent))
    assert "venv/lib/site-packages/dep/__init__.py" in _files(keyed.trace(keyed.records[0]))
    assert not any(n.startswith("venv/") for n in _files(skipped.trace(skipped.records[0])))


# ------------------------------------------------------------------------------ the closure fixes
def test_a_file_depends_on_the_init_of_its_own_package(tmp_path, monkeypatch):
    """TID-128: importing `pkg.sub.mod` runs both `__init__.py` files, and no import statement in
    `mod.py` says so."""
    root = _tree(tmp_path, {"pkg/__init__.py": "", "pkg/sub/__init__.py": "", "pkg/sub/mod.py": "X = 1\n",
                            "ns/plain.py": "", "test_m.py": "from pkg.sub.mod import X\nimport ns.plain\n"})
    monkeypatch.syspath_prepend(root)
    closure = footprint._import_closure(footprint.Caches(), "test_m.py", root)
    assert closure == {"pkg/sub/mod.py", "pkg/sub/__init__.py", "pkg/__init__.py", "ns/plain.py"}
    assert footprint._package_init(os.path.join(root, "test_m.py"), root) is None  # no package at the root
    assert footprint._package_init(os.path.join(root, "pkg", "__init__.py"), root) is None


def test_the_closure_follows_what_a_conftest_imports_and_can_cover_more_than_the_run_root(tmp_path, monkeypatch):
    root = _tree(tmp_path, {"conftest.py": "import helper\n", "helper.py": "", "src/lib.py": "",
                            "tests/conftest.py": "", "tests/test_x.py": "import src.lib\n"})
    monkeypatch.syspath_prepend(root)
    tests = os.path.join(root, "tests")
    inside = footprint._import_closure(footprint.Caches(), "test_x.py", tests)
    assert inside == {"conftest.py"}, "the run root's own view: nothing above it"
    wide = footprint._import_closure(footprint.Caches(), "test_x.py", tests, scope=root)
    assert wide == {"conftest.py", "helper.py", "tests/conftest.py", "src/lib.py"}
    cov = footprint._Coverage(tests, True, scope=root)
    assert cov.root == root and cov.run_root == tests
    assert cov.report_with_imports("test_x.py").keys() == wide


# ------------------------------------------------------------------------------ the binding
def test_without_the_binding_the_error_says_what_to_install(monkeypatch, capsys):
    monkeypatch.setitem(sys.modules, "estuary_etf", None)  # `import estuary_etf` raises ImportError
    with pytest.raises(etf.EtfUnavailable, match=r"pip install 'tiderace\[estuary\]'"):
        etf.binding()
    assert etf.main(["--etf-check"]) == etf.EXIT_NO_BINDING
    assert etf.main(["--etf-emit", "/nonexistent/manifest.json"]) == 1
    err = capsys.readouterr().err
    assert "estuary_etf" in err and "FileNotFoundError" in err


def _manifest(project, out, tests):
    return {"v": 1, "out": str(out), "run_root": project, "trace_root": project, "engine_version": "0.0.1",
            "created_at_ms": 1_700_000_000_000, "tests": tests}


def test_the_written_file_decodes_to_the_plan(project, tmp_path, capsys):
    """TID-131: valid under the v0 reader, one record per node, the plan's keys with a value each."""
    binding = pytest.importorskip("estuary_etf", reason="the optional estuary-etf wheel is not installed")
    out = tmp_path / "run.etf"
    tests = [{"id": "tests/unit/test_a.py::t[x]", "outcome": "passed", "duration_ms": 3,
              "reads": {"file": ["tests/unit/test_a.py", "gone.py"]}},
             {"id": "tests/other/test_c.py::t", "outcome": "failed", "duration_ms": 0, "reads": {"file": []}}]
    manifest = _manifest(project, out, tests)
    assert etf.main(["--etf-emit", _write_json(tmp_path, manifest)]) == 0
    assert "2 records" in capsys.readouterr().out
    data = out.read_bytes()
    decoded = binding.decode(data, verify=True)
    assert decoded.encode() == data
    header = decoded.header
    assert (header["harness"], header["u_version"], header["granularity"]) == ("tiderace", "", "file")
    assert header["created_at_ms"] == 1_700_000_000_000 and header["tree"] == bytes(32)
    records = {r["check"].decode(): r for r in decoded.records}
    assert set(records) == {"tests/unit/test_a.py::t[x]", "tests/other/test_c.py::t"}
    assert records["tests/other/test_c.py::t"]["outcome"] == "fail"
    assert records["tests/unit/test_a.py::t[x]"]["duration_us"] == 3000

    pairs = {(kind, name.decode()): value for kind, name, value in decoded.flatten(records["tests/unit/test_a.py::t[x]"]["root"])}
    assert pairs[("file", "pkg/core.py")] == binding.value_hash("file", b"from pkg import util\n")
    assert pairs[("file", "gone.py")] is None and pairs[("file", "tests/conftest.py")] is None
    assert pairs[("derived", "cfg/pytest.ini#pytest")] == binding.value_hash("derived", b'{"addopts":"-q"}')
    assert pairs[("tool", "tiderace")] is not None
    children, own = decoded.fragment(records["tests/unit/test_a.py::t[x]"]["root"])
    assert header["toolchain_fragment"] in children and [n for _, n, _ in own] == [b"gone.py"]

    # The same run again is the same bytes; an edited file is a different trace for who reads it.
    assert etf.main(["--etf-emit", _write_json(tmp_path, manifest)]) == 0
    assert out.read_bytes() == data
    before = {check: decoded.trace_digest(r["root"]) for check, r in records.items()}
    (tmp_path / "pkg" / "core.py").write_text("from pkg import util\nEDITED = 1\n")
    assert etf.main(["--etf-emit", _write_json(tmp_path, manifest)]) == 0
    again = binding.decode(out.read_bytes())
    after = {r["check"].decode(): again.trace_digest(r["root"]) for r in again.records}
    assert after["tests/unit/test_a.py::t[x]"] != before["tests/unit/test_a.py::t[x]"]
    assert after["tests/other/test_c.py::t"] == before["tests/other/test_c.py::t"]


def test_a_manifest_it_does_not_know_is_refused(project, tmp_path, capsys):
    pytest.importorskip("estuary_etf", reason="the optional estuary-etf wheel is not installed")
    manifest = dict(_manifest(project, tmp_path / "run.etf", []), v=99)
    assert etf.main(["--etf-emit", _write_json(tmp_path, manifest)]) == 1
    assert "unknown manifest version" in capsys.readouterr().err


def _write_json(tmp_path, manifest) -> str:
    import json

    path = tmp_path / "manifest.json"
    path.write_text(json.dumps(manifest))
    return str(path)
