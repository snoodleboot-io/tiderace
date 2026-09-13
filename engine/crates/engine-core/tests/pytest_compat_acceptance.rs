//! TID-44 — pytest compatibility across versions: fixtures, `addfinalizer`, and `monkeypatch._setitem`.
//!
//! Found by the parity pass for a benchmark against the pinned conformance repos. Every corpus the
//! engine had been validated against ran pytest 9, and the first suites pinning older pytest broke
//! wholesale: click 8.1.7 (pytest 7.4) passed 227 of 589 tests, flask 3.0.3 (pytest 8.1) passed none.
//!
//! **Fixtures were invisible before pytest 8.4.** pytest moved its fixture representation in 8.4:
//!
//! | pytest  | marker                     | real function            |
//! | ------- | -------------------------- | ------------------------ |
//! | < 8.4   | `_pytestfixturefunction`   | `__pytest_wrapped__.obj` |
//! | >= 8.4  | `_fixture_function_marker` | `_fixture_function`      |
//!
//! Only the new names were recognised. The test below reproduces the pre-8.4 layout by hand rather
//! than installing an old pytest, because CI provisions one pytest version. That checks the logic;
//! the real-world confirmation was click and flask on their own pinned pytest 7.4 and 8.1.
//!
//! **`request.addfinalizer` did not exist** on either the fixture or the test request, and flask's
//! fixtures use it for cleanup.
//!
//! **`monkeypatch._setitem` did not exist.** It is pytest's *private* undo log, but flask's conftest
//! appends a session's worth of environ records to it so every test resets `os.environ`. That autouse
//! fixture failed setup for 451 of flask's 482 tests.

use engine_core::collection::{Collector, RegexCollector};
use engine_core::domain::Outcome;
use engine_core::exec::{SubprocessWorker, Worker};
use engine_core::testing::skip_live;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repo root")
}

fn shim() -> PathBuf {
    repo_root().join("engine/py-shim/shim.py")
}

/// These corpora define real pytest fixtures, so they need an interpreter that has pytest. Prefer
/// the fx venv, which CI provisions for exactly this.
fn python_with_pytest() -> Option<String> {
    python_that_imports("import pytest")
}

/// pytest *and* the tiderace builtins — the `monkeypatch` test needs both. Resolved through the
/// spawned interpreter's own import path, exactly as `builtins_acceptance` does, so it runs wherever
/// CI puts `engine/py-tiderace` on `PYTHONPATH` and skips (loudly) where nothing does.
fn python_with_pytest_and_builtins() -> Option<String> {
    python_that_imports("import pytest, tiderace.builtins")
}

fn python_that_imports(statement: &str) -> Option<String> {
    let venv = repo_root().join(".tiderace-fx-venv/bin/python");
    let candidates: Vec<String> = if venv.exists() {
        vec![venv.to_string_lossy().into_owned()]
    } else {
        vec!["python3".into(), "python".into()]
    };
    candidates.into_iter().find(|p| {
        std::process::Command::new(p)
            .args(["-c", statement])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    })
}

fn scratch(tag: &str) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "tiderace_t44_{tag}_{}_{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn run_all(dir: &Path, python: String) -> Vec<engine_core::domain::TestResult> {
    let items = RegexCollector::new().collect(dir).expect("collection");
    let mut worker = SubprocessWorker::new(30_000, 1).with_target(python, &shim(), dir);
    worker.run(&items).expect("batch runs")
}

fn assert_all_passed(results: &[engine_core::domain::TestResult], why: &str) {
    assert!(!results.is_empty(), "the corpus collected nothing");
    for r in results {
        assert_eq!(
            r.outcome,
            Outcome::Passed,
            "{why}: {} — {}",
            r.node_id.as_str(),
            r.detail
        );
    }
}

/// A fixture in pytest's pre-8.4 layout is found and run through its real function, not its wrapper.
///
/// The decorated object is a wrapper whose job is to raise when called directly; the hand-built one
/// does the same, so resolving to it instead of `__pytest_wrapped__.obj` would fail the test rather
/// than pass by accident.
#[test]
fn a_fixture_in_the_pre_8_4_layout_is_recognised() {
    let Some(python) = python_with_pytest() else {
        skip_live("no interpreter with pytest available");
        return;
    };
    let dir = scratch("oldfixture");
    std::fs::write(
        dir.join("conftest.py"),
        r#"
class _Marker:
    """Stands in for pytest < 8.4's FixtureFunctionMarker — same fields, same meaning."""
    scope = "function"
    params = None
    autouse = False
    ids = None
    name = None


def runner():
    return 42


# pytest's `__pytest_wrapped__` is a frozen dataclass holding `obj` as an *instance* attribute. A class
# attribute would come back as a bound method, which is not what pytest produces.
from types import SimpleNamespace


def _decorated_runner():
    raise RuntimeError("fixture called directly — resolved the wrapper, not the real function")


_decorated_runner.__name__ = "runner"
_decorated_runner._pytestfixturefunction = _Marker()
_decorated_runner.__pytest_wrapped__ = SimpleNamespace(obj=runner)
runner = _decorated_runner
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("test_old_fixture.py"),
        "def test_receives_the_fixture_value(runner):\n    assert runner == 42\n",
    )
    .unwrap();

    let results = run_all(&dir, python);
    assert_all_passed(
        &results,
        "TID-44: a pre-8.4 fixture must be recognised and resolved to its real function",
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// `request.addfinalizer` works on both request types, at the right time and in pytest's order.
///
/// Observed through a **file**, not a module global. tiderace isolates tests from each other's module
/// state — the in-process ladder restores the test module's globals after the body, and a fork
/// discards them — so a finalizer appending to an imported list would be rolled back or thrown away,
/// and a test asserting otherwise would be asserting pytest's *lack* of isolation. The disk is outside
/// both mechanisms, so it records what actually ran.
///
/// The session fixture's finalizer must *not* run between tests — tying it to the fixture's own
/// teardown is what keeps it at session scope. Within one fixture the yield teardown runs first, then
/// finalizers newest first, because pytest registers the yield teardown after the body returns.
#[test]
fn addfinalizer_runs_at_the_fixtures_own_teardown_in_pytest_order() {
    let Some(python) = python_with_pytest() else {
        skip_live("no interpreter with pytest available");
        return;
    };
    let dir = scratch("finalizer");
    let log = dir.join("finalizers.log");
    std::fs::write(
        dir.join("conftest.py"),
        format!(
            r#"
import pytest

LOG = {log:?}


def record(event):
    with open(LOG, "a", encoding="utf-8") as fh:
        fh.write(event + "\n")


@pytest.fixture(scope="session")
def session_thing(request):
    request.addfinalizer(lambda: record("session-finalizer"))
    return "s"


@pytest.fixture
def ordered(request):
    request.addfinalizer(lambda: record("fin-1"))
    request.addfinalizer(lambda: record("fin-2"))
    yield "o"
    record("yield-teardown")
"#
        ),
    )
    .unwrap();
    std::fs::write(
        dir.join("test_finalizers.py"),
        format!(
            r#"
import os

from conftest import record

LOG = {log:?}


def events():
    return open(LOG, encoding="utf-8").read().split() if os.path.exists(LOG) else []


def test_a(session_thing, ordered, request):
    request.addfinalizer(lambda: record("test-finalizer"))
    assert events() == []


def test_b(session_thing, ordered):
    # test_a's function-scope cleanup ran in pytest's order; the session finalizer has not run yet.
    assert events() == ["test-finalizer", "yield-teardown", "fin-2", "fin-1"], events()
"#
        ),
    )
    .unwrap();

    let results = run_all(&dir, python);
    assert_all_passed(
        &results,
        "TID-44: addfinalizer must run at the fixture's own teardown, in pytest's order",
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// pytest-format records appended to `monkeypatch._setitem` are undone — flask's pattern, verbatim.
///
/// A session fixture builds pytest's *own* `MonkeyPatch`, records a key that did not exist, and hands
/// back its `_setitem` list; a function fixture appends that to the injected `monkeypatch`. When the
/// test's teardown runs, the key must be deleted — which requires recognising pytest's "did not
/// exist" sentinel, and that sentinel changed name and type between pytest 8 and 9.
#[test]
fn pytest_format_setitem_records_are_undone() {
    let Some(python) = python_with_pytest_and_builtins() else {
        skip_live(
            "no interpreter can import both pytest and `tiderace` — put engine/py-tiderace on \
             PYTHONPATH (CI does)",
        );
        return;
    };
    let dir = scratch("setitem");
    std::fs::write(dir.join("state_probe.py"), "STATE = {}\n").unwrap();
    std::fs::write(
        dir.join("conftest.py"),
        r#"
import os
import sys

sys.path.insert(0, os.path.dirname(__file__))

import pytest
from _pytest import monkeypatch as pytest_monkeypatch

from state_probe import STATE


@pytest.fixture(scope="session")
def recorded():
    mp = pytest_monkeypatch.MonkeyPatch()
    mp.setitem(STATE, "leaked", True)   # records (STATE, "leaked", <pytest's not-set sentinel>)
    records = list(mp._setitem)
    STATE.pop("leaked", None)
    return records


@pytest.fixture(autouse=True)
def reset(monkeypatch, recorded):
    monkeypatch._setitem.extend(recorded)
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("test_setitem.py"),
        r#"
from state_probe import STATE


def test_a_sets_it():
    STATE["leaked"] = True


def test_b_it_was_undone():
    assert "leaked" not in STATE, STATE
"#,
    )
    .unwrap();

    let results = run_all(&dir, python);
    assert_all_passed(
        &results,
        "TID-44: pytest-format `_setitem` records must be undone at teardown",
    );
    let _ = std::fs::remove_dir_all(&dir);
}
