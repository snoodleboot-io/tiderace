//! TID-64 — `setUpClass` runs once per class per process, `tearDownClass` once at worker end.
//!
//! The shim used to run both around *every* method. That was right when every test forked — each
//! child was its own process — and wrong under the in-process ladder, where a class's methods run in
//! one process one after another: N× the setup cost, and a `setUpClass` that opens a database or
//! counts its own calls behaved differently from `python -m unittest` and from pytest. pytest's own
//! `setup_class` was already gated once per class (TID-60); the unittest dialect was left behind, and
//! `teardown_class` was never called at all.
//!
//! One worker, one process, the whole corpus: the subprocess tier, so "once per process" is
//! observable as "once". Every hook appends a line to a file on disk — a module-level counter would
//! be rolled back by the in-process restore.

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

fn any_python() -> Option<String> {
    let venv = repo_root().join(".tiderace-fx-venv/bin/python");
    if venv.exists() {
        return Some(venv.to_string_lossy().into_owned());
    }
    ["python3", "python"]
        .into_iter()
        .find(|cand| {
            std::process::Command::new(cand)
                .arg("--version")
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        })
        .map(str::to_string)
}

fn scratch(tag: &str) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "tiderace_t64_{tag}_{}_{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Both dialects, three methods each, every class-level hook logging to `hooks.log`.
fn write_project(tag: &str) -> PathBuf {
    let dir = scratch(tag);
    let tests = dir.join("tests");
    std::fs::create_dir_all(&tests).unwrap();
    let log = tests.join("hooks.log").to_string_lossy().into_owned();
    std::fs::write(
        tests.join("conftest.py"),
        format!("LOG = {log:?}\n\ndef note(what):\n    with open(LOG, 'a') as fh:\n        fh.write(what + '\\n')\n"),
    )
    .unwrap();
    std::fs::write(
        tests.join("test_unittest_style.py"),
        "import unittest\nfrom conftest import note\n\n\
         class TestDb(unittest.TestCase):\n\
         \x20   @classmethod\n\
         \x20   def setUpClass(cls):\n\
         \x20       note('unittest setUpClass')\n\
         \x20       cls.conn = object()\n\
         \x20   @classmethod\n\
         \x20   def tearDownClass(cls):\n\
         \x20       note('unittest tearDownClass')\n\
         \x20   def test_a(self):\n\
         \x20       self.assertIsNotNone(self.conn)\n\
         \x20   def test_b(self):\n\
         \x20       self.assertIsNotNone(self.conn)\n\
         \x20   def test_c(self):\n\
         \x20       self.assertIsNotNone(self.conn)\n",
    )
    .unwrap();
    std::fs::write(
        tests.join("test_pytest_style.py"),
        "from conftest import note\n\n\
         class TestApi:\n\
         \x20   @classmethod\n\
         \x20   def setup_class(cls):\n\
         \x20       note('pytest setup_class')\n\
         \x20   @classmethod\n\
         \x20   def teardown_class(cls):\n\
         \x20       note('pytest teardown_class')\n\
         \x20   def test_x(self):\n\
         \x20       assert True\n\
         \x20   def test_y(self):\n\
         \x20       assert True\n\
         \x20   def test_z(self):\n\
         \x20       assert True\n",
    )
    .unwrap();
    dir
}

fn run(python: &str, tests: &Path) -> Vec<engine_core::domain::TestResult> {
    let items = RegexCollector::new().collect(tests).expect("collection");
    let mut worker = SubprocessWorker::new(20_000, 1).with_target(python, &shim(), tests);
    let results = worker.run(&items).expect("batch runs");
    drop(worker); // EOF → the shim's `teardown_all`, where class and module teardowns run
    results
}

fn count(log: &str, line: &str) -> usize {
    log.lines().filter(|l| *l == line).count()
}

#[test]
fn class_setup_runs_once_per_class_and_teardown_once_at_worker_end() {
    let Some(python) = any_python() else {
        skip_live("no Python interpreter available");
        return;
    };
    let dir = write_project("once");
    let tests = dir.join("tests");
    let results = run(&python, &tests);
    assert_eq!(results.len(), 6, "{results:?}");
    for r in &results {
        assert_eq!(r.outcome, Outcome::Passed, "{}: {}", r.node_id, r.detail);
    }

    let log = std::fs::read_to_string(tests.join("hooks.log")).expect("hooks logged");
    assert_eq!(
        count(&log, "unittest setUpClass"),
        1,
        "TID-64: setUpClass once per class per process, not once per method — log:\n{log}"
    );
    assert_eq!(
        count(&log, "unittest tearDownClass"),
        1,
        "and torn down once — log:\n{log}"
    );
    assert_eq!(count(&log, "pytest setup_class"), 1, "log:\n{log}");
    assert_eq!(
        count(&log, "pytest teardown_class"),
        1,
        "teardown_class was never called before TID-64 — log:\n{log}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Once-per-class also means the *outcome* of a failed setup is the class's, not the first
/// method's. unittest skips every method when `setUpClass` skips and errors every method when it
/// raises; so does pytest. A gate that ran setup once and then let the other methods through would
/// run them against a class that never set up.
#[test]
fn a_failed_class_setup_decides_every_method_and_runs_once() {
    let Some(python) = any_python() else {
        skip_live("no Python interpreter available");
        return;
    };
    let dir = scratch("failed");
    let tests = dir.join("tests");
    std::fs::create_dir_all(&tests).unwrap();
    let log = tests.join("hooks.log").to_string_lossy().into_owned();
    std::fs::write(
        tests.join("test_setup_fails.py"),
        format!(
            "import unittest\n\nLOG = {log:?}\n\n\
             def note(what):\n    with open(LOG, 'a') as fh:\n        fh.write(what + '\\n')\n\n\
             class TestSkips(unittest.TestCase):\n\
             \x20   @classmethod\n\
             \x20   def setUpClass(cls):\n\
             \x20       note('skips setUpClass')\n\
             \x20       raise unittest.SkipTest('no backend')\n\
             \x20   def test_a(self):\n        assert False\n\
             \x20   def test_b(self):\n        assert False\n\n\
             class TestRaises(unittest.TestCase):\n\
             \x20   @classmethod\n\
             \x20   def setUpClass(cls):\n\
             \x20       note('raises setUpClass')\n\
             \x20       raise RuntimeError('database is down')\n\
             \x20   def test_a(self):\n        assert True\n\
             \x20   def test_b(self):\n        assert True\n"
        ),
    )
    .unwrap();
    let results = run(&python, &tests);
    assert_eq!(results.len(), 4, "{results:?}");
    for r in &results {
        let id = r.node_id.as_str();
        if id.contains("TestSkips") {
            assert_eq!(r.outcome, Outcome::Skipped, "{id}: {}", r.detail);
        } else {
            assert_eq!(r.outcome, Outcome::Error, "{id}: {}", r.detail);
            assert!(r.detail.contains("database is down"), "{id}: {}", r.detail);
        }
    }
    let log = std::fs::read_to_string(tests.join("hooks.log")).expect("hooks logged");
    assert_eq!(
        count(&log, "skips setUpClass"),
        1,
        "attempted once, not per method — log:\n{log}"
    );
    assert_eq!(count(&log, "raises setUpClass"), 1, "log:\n{log}");
    let _ = std::fs::remove_dir_all(&dir);
}
