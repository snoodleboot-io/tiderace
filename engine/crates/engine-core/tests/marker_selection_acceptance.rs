//! TID-59 — marks: declared, validated, and selectable in both dialects.
//!
//! Marks existed in pieces. pytest marks could be filtered, but only by an `-m` expression buried in
//! the project's `addopts`; native `@tiderace.tag` was inert, its own docstring admitting nothing
//! consumed it; `markers = [...]` was never read, so nothing knew which marks a project had declared;
//! and `--strict-markers` was parsed out of `addopts` and dropped. That last one is the worst of the
//! four: a project asks for validation, and a typo'd `@pytest.mark.slwo` then runs a test its author
//! had filtered out, silently.
//!
//! The rule this pins down is that **selection means the same thing in both dialects**. One
//! expression evaluator, one set of names — pytest marks and tiderace tags together — so
//! `-m "not slow"` deselects `@pytest.mark.slow` and `@tiderace.mark.slow` alike. Two selection
//! languages would be a fork in the road for anyone migrating a suite a file at a time.
//!
//! Deselected tests are absent from the tally, as `-m` deselection already was: pytest does not
//! collect them, so reporting them as skips would invent an outcome.

#![cfg(unix)]

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

/// Needs pytest (for `@pytest.mark`) and tiderace (for `@tiderace.mark`) — the point is both at once.
fn python_with_both() -> Option<String> {
    let venv = repo_root().join(".tiderace-fx-venv/bin/python");
    let mut candidates: Vec<String> = Vec::new();
    if venv.exists() {
        candidates.push(venv.to_string_lossy().into_owned());
    }
    candidates.extend(["python3".to_string(), "python".to_string()]);
    candidates.into_iter().find(|p| {
        std::process::Command::new(p)
            .args(["-c", "import pytest, tiderace"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    })
}

fn scratch(tag: &str) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "tiderace_t59_{tag}_{}_{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

const CORPUS: &str = r#"
import pytest

import tiderace


@pytest.mark.slow
def test_marked_the_pytest_way():
    assert True


@tiderace.mark.slow
def test_marked_the_native_way():
    assert True


def test_unmarked():
    assert True
"#;

/// Both dialects declared, so neither is "unknown" when strict checking is on.
fn write_project(dir: &Path, addopts: &str) {
    std::fs::write(
        dir.join("pyproject.toml"),
        format!(
            "[tool.pytest.ini_options]\nmarkers = [\"slow: takes real time\"]\naddopts = \"{addopts}\"\n"
        ),
    )
    .unwrap();
    std::fs::write(dir.join("test_marks.py"), CORPUS).unwrap();
}

fn run(dir: &Path, python: &str) -> Vec<engine_core::domain::TestResult> {
    let items = RegexCollector::new().collect(dir).expect("collection");
    SubprocessWorker::new(20_000, 1)
        .with_target(python.to_string(), &shim(), dir)
        .run(&items)
        .expect("batch runs")
}

/// One test function, because the marker expression travels through the environment and the
/// environment is process-wide: running these as separate `#[test]`s would let them race.
#[test]
fn marks_are_selectable_in_both_dialects_and_validated_when_asked() {
    let Some(python) = python_with_both() else {
        skip_live("no interpreter with both pytest and tiderace");
        return;
    };

    // ── selection reaches both dialects ──────────────────────────────────────────────────────
    let dir = scratch("select");
    write_project(&dir, "-ra");
    // SAFETY: the suite's env-dependent assertions all live in this one test.
    unsafe { std::env::set_var("TIDERACE_MARKER_EXPR", "not slow") };
    let results = run(&dir, &python);
    let ran: Vec<&str> = results
        .iter()
        .filter(|r| !matches!(r.outcome, Outcome::Skipped))
        .map(|r| r.node_id.as_str())
        .filter(|id| !id.ends_with("]"))
        .collect();
    assert_eq!(
        ran,
        vec!["test_marks.py::test_unmarked"],
        "TID-59: `-m 'not slow'` must deselect the pytest-marked *and* the natively-marked test, \
         and deselected tests must be absent from the tally rather than reported as skips"
    );

    unsafe { std::env::set_var("TIDERACE_MARKER_EXPR", "slow") };
    let results = run(&dir, &python);
    let mut ran: Vec<&str> = results.iter().map(|r| r.node_id.as_str()).collect();
    ran.sort();
    assert_eq!(
        ran,
        vec![
            "test_marks.py::test_marked_the_native_way",
            "test_marks.py::test_marked_the_pytest_way",
        ],
        "TID-59: `-m slow` keeps exactly the marked tests of both dialects"
    );
    unsafe { std::env::remove_var("TIDERACE_MARKER_EXPR") };
    let _ = std::fs::remove_dir_all(&dir);

    // ── --strict-markers rejects a mark the project never declared ───────────────────────────
    let strict = scratch("strict");
    write_project(&strict, "--strict-markers");
    std::fs::write(
        strict.join("test_typo.py"),
        "import pytest\n\n\n@pytest.mark.slwo\ndef test_typo():\n    assert True\n",
    )
    .unwrap();
    let results = run(&strict, &python);

    let typo = results
        .iter()
        .find(|r| r.node_id.as_str().contains("test_typo"))
        .expect("the typo'd test reports");
    assert_eq!(
        typo.outcome,
        Outcome::Error,
        "TID-59: under --strict-markers an undeclared mark is an error, not a silently-running test"
    );
    assert!(
        typo.detail.contains("slwo"),
        "the error has to name the offending mark, or it cannot be acted on — got {}",
        typo.detail
    );
    // The declared marks in the same run are untouched.
    for r in &results {
        if r.node_id.as_str().contains("test_marked")
            || r.node_id.as_str().contains("test_unmarked")
        {
            assert_eq!(
                r.outcome,
                Outcome::Passed,
                "a declared mark must not be rejected — {} {}",
                r.node_id.as_str(),
                r.detail
            );
        }
    }
    let _ = std::fs::remove_dir_all(&strict);
}
