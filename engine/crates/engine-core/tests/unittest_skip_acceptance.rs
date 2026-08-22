//! TID-16 — pytest's `Skipped` is a skip on a `TestCase`, not an error.
//!
//! `unittest`'s executor special-cases exactly one skip type, `unittest.SkipTest`. pytest's `skip()`
//! and `importorskip()` raise `_pytest.outcomes.Skipped`, which derives from `BaseException` and so
//! falls through the executor's bare `except:` into `addError`. On any suite with optional extras
//! that turned a clean run red — `pytest.importorskip("optional_dep")` is *the* idiom for "skip when
//! this extra is absent", and tiderace called it a defect.
//!
//! The corpus deliberately skips on a module that cannot exist, so the skip is real rather than
//! contingent on what happens to be installed. Requires pytest (nothing here means anything without
//! it), so it self-skips on a bare interpreter.

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

fn any_python() -> Option<String> {
    let venv = repo_root().join(".tiderace-fx-venv/bin/python");
    if venv.exists() {
        return Some(venv.to_string_lossy().into_owned());
    }
    for cand in ["python3", "python"] {
        let ok = std::process::Command::new(cand)
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if ok {
            return Some(cand.to_string());
        }
    }
    None
}

/// An interpreter that can `import pytest`. Windows CI's bare `actions/setup-python` cannot.
fn python_with_pytest() -> Option<String> {
    let python = any_python()?;
    let ok = std::process::Command::new(&python)
        .args(["-c", "import pytest"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    ok.then_some(python)
}

/// Both `TestCase` flavours, both skip idioms, a skip from `setUp`, and — the guard that matters —
/// a genuine error and a genuine failure that must NOT be swallowed into "skipped".
const CORPUS: &str = "\
import unittest

import pytest


class TestSyncSkips(unittest.TestCase):
    def test_importorskip_is_a_skip(self):
        pytest.importorskip(\"a_module_that_cannot_possibly_exist\")
        raise AssertionError(\"unreachable: importorskip should have skipped\")

    def test_explicit_skip_is_a_skip(self):
        pytest.skip(\"not applicable here\")

    def test_unittest_skiptest_still_works(self):
        raise unittest.SkipTest(\"the pre-existing path\")

    def test_real_error_is_still_an_error(self):
        raise RuntimeError(\"boom\")

    def test_real_failure_is_still_a_failure(self):
        assert 1 == 2


class TestAsyncSkips(unittest.IsolatedAsyncioTestCase):
    async def test_importorskip_is_a_skip_async(self):
        pytest.importorskip(\"a_module_that_cannot_possibly_exist\")
        raise AssertionError(\"unreachable: importorskip should have skipped\")

    async def test_real_error_is_still_an_error_async(self):
        raise RuntimeError(\"boom\")


class TestSkipFromSetUp(unittest.TestCase):
    def setUp(self):
        pytest.skip(\"skipped during setUp\")

    def test_body_never_runs(self):
        raise AssertionError(\"unreachable: setUp should have skipped\")
";

/// Expected outcome, from the leaf name. Encoded in the names so the corpus stays self-describing.
fn expected(node_id: &str) -> Outcome {
    let leaf = node_id.rsplit("::").next().unwrap_or(node_id);
    if leaf.contains("error") {
        // A body that raises is a FAILURE, as pytest reports it (TID-30). The guard this corpus
        // exists for is unchanged: a raising body must not be swallowed into "skipped".
        Outcome::Failed
    } else if leaf.contains("failure") {
        Outcome::Failed
    } else {
        Outcome::Skipped
    }
}

fn write_corpus(tag: &str) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "tiderace_uskip_{tag}_{}_{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("test_unittest_skip.py"), CORPUS).unwrap();
    dir
}

#[test]
fn pytest_skips_inside_a_testcase_are_skips_not_errors() {
    let Some(python) = python_with_pytest() else {
        skip_live("no interpreter with pytest available");
        return;
    };
    let dir = write_corpus("run");
    let items = RegexCollector::new().collect(&dir).expect("collection");
    assert_eq!(items.len(), 8, "8 tests across the three classes");

    let mut worker = SubprocessWorker::new(10_000, 1).with_target(python, &shim(), &dir);
    let results = worker.run(&items).expect("batch runs against real Python");
    assert_eq!(results.len(), 8, "one result per test");

    for r in &results {
        assert_eq!(
            r.outcome,
            expected(r.node_id.as_str()),
            "TID-16: wrong outcome for {}; detail: {}",
            r.node_id,
            r.detail
        );
    }

    // The skip reason has to survive, or a skipped run says nothing about why.
    let importorskip = results
        .iter()
        .find(|r| r.node_id.as_str().ends_with("test_importorskip_is_a_skip"))
        .expect("the importorskip test is reported");
    assert!(
        importorskip
            .detail
            .contains("a_module_that_cannot_possibly_exist"),
        "the skip must carry its reason; got: {:?}",
        importorskip.detail
    );
    let _ = std::fs::remove_dir_all(&dir);
}
