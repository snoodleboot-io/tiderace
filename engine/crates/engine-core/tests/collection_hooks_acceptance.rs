//! TID-20 — conftest collection hooks and `@pytest.mark.skip` are honoured.
//!
//! Suites gate optional backends in `pytest_collection_modifyitems`: mark the tests `needs_kuzu`,
//! then add a skip marker to each unless `--real` was passed. tiderace ignored the hook, so those
//! tests ran anyway and died on the missing import — a red run for a dependency the suite
//! deliberately made optional, where pytest reports a skip.
//!
//! The skip has to happen **before fixture setup**: a test skipped for a missing backend must not
//! pay to build one. That is asserted here rather than assumed, via a fixture that fails loudly if
//! it is ever set up.
//!
//! Needs pytest (there is no `pytest.mark` without it), so it self-skips on a bare interpreter.

use engine_core::collection::{Collector, RegexCollector};
use engine_core::domain::Outcome;
use engine_core::exec::{SubprocessWorker, Worker};
use engine_core::testing::skip_live;
use std::path::PathBuf;
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

fn python_with_pytest() -> Option<String> {
    let venv = repo_root().join(".tiderace-fx-venv/bin/python");
    let mut candidates: Vec<String> = Vec::new();
    if venv.exists() {
        candidates.push(venv.to_string_lossy().into_owned());
    }
    candidates.extend(["python3".to_string(), "python".to_string()]);
    candidates.into_iter().find(|cand| {
        std::process::Command::new(cand)
            .args(["-c", "import pytest"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    })
}

/// The shape real suites use: an opt-in flag, plus a hook that skips marked tests without it.
const CONFTEST: &str = "\
import pytest


def pytest_addoption(parser):
    parser.addoption(\"--real\", action=\"store_true\", default=False, help=\"run real-backend tests\")


def pytest_collection_modifyitems(config, items):
    if config.getoption(\"--real\"):
        return
    reasons = {\"needs_backend\": \"pass --real to run backend tests\"}
    for item in items:
        for marker, reason in reasons.items():
            if marker in item.keywords:
                item.add_marker(pytest.mark.skip(reason=reason))
";

/// `guard` raises if it is ever set up, which pins that a marker skip short-circuits before fixture
/// setup rather than after. A test that skipped *after* paying for its backend would still report
/// SKIP, so only this can tell the difference.
const CORPUS: &str = "\
import pytest


@pytest.fixture
def guard():
    raise AssertionError(\"fixture setup ran for a test that should have been skipped\")


@pytest.mark.needs_backend
def test_hook_skips_a_marked_function(guard):
    raise AssertionError(\"body ran for a skipped test\")


@pytest.mark.needs_backend
class TestMarkedClass:
    def test_hook_skips_a_marked_method(self, guard):
        raise AssertionError(\"body ran for a skipped test\")


@pytest.mark.skip(reason=\"skipped directly\")
def test_direct_skip_marker(guard):
    raise AssertionError(\"body ran for a skipped test\")


@pytest.mark.skipif(True, reason=\"condition is true\")
def test_skipif_true(guard):
    raise AssertionError(\"body ran for a skipped test\")


@pytest.mark.skipif(False, reason=\"condition is false\")
def test_skipif_false():
    assert True


@pytest.mark.needs_something_else
def test_unrelated_marker_still_runs():
    assert True


def test_unmarked_still_runs():
    assert True
";

fn write_corpus(tag: &str) -> (PathBuf, PathBuf) {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let pkg = std::env::temp_dir().join(format!(
        "tiderace_hooks_{tag}_{}_{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&pkg);
    let tests = pkg.join("tests");
    std::fs::create_dir_all(&tests).unwrap();
    std::fs::write(
        pkg.join("pyproject.toml"),
        "[project]\nname = \"hook-probe\"\n",
    )
    .unwrap();
    // The hook lives in the ANCESTOR conftest, where suites really put it — which also pins that
    // TID-19's collection path feeds the hook host.
    std::fs::write(pkg.join("conftest.py"), CONFTEST).unwrap();
    std::fs::write(tests.join("test_hooks.py"), CORPUS).unwrap();
    (pkg, tests)
}

/// Expected outcome from the leaf name: anything named `..._still_runs` or `..._false` passes,
/// everything else is skipped.
fn expected(node_id: &str) -> Outcome {
    let leaf = node_id.rsplit("::").next().unwrap_or(node_id);
    if leaf.ends_with("still_runs") || leaf.ends_with("skipif_false") {
        Outcome::Passed
    } else {
        Outcome::Skipped
    }
}

#[test]
fn collection_hooks_and_skip_markers_are_honoured() {
    let Some(python) = python_with_pytest() else {
        skip_live("no interpreter with pytest available");
        return;
    };
    let (pkg, run_root) = write_corpus("run");
    let items = RegexCollector::new()
        .collect(&run_root)
        .expect("collection");
    assert_eq!(items.len(), 7, "7 tests in the corpus");

    let mut worker = SubprocessWorker::new(10_000, 1).with_target(python, &shim(), &run_root);
    let results = worker.run(&items).expect("batch runs against real Python");
    assert_eq!(results.len(), 7, "one result per test");

    for r in &results {
        assert_eq!(
            r.outcome,
            expected(r.node_id.as_str()),
            "TID-20: wrong outcome for {}; detail: {}",
            r.node_id,
            r.detail
        );
    }

    // The reason has to survive, or a skipped run cannot say why it skipped.
    let hooked = results
        .iter()
        .find(|r| {
            r.node_id
                .as_str()
                .ends_with("test_hook_skips_a_marked_function")
        })
        .expect("the hook-skipped test is reported");
    assert!(
        hooked.detail.contains("--real"),
        "the hook's reason must reach the report; got {:?}",
        hooked.detail
    );
    let _ = std::fs::remove_dir_all(&pkg);
}
