//! TID-63 — `@pytest.mark.xfail` written above a test reaches the outcome.
//!
//! The xfail fold read tiderace's *native* marks only, so a test carrying pytest's own
//! `@pytest.mark.xfail` was reported as a plain failure. click's `test_multicommand_chaining` is
//! marked expected-to-fail and fails exactly as its author intended; tiderace called the run red for
//! it. A failure the author already accounted for is not news, and reporting it as news is how a
//! green suite starts looking broken.
//!
//! The runtime path (`request.node.add_marker(pytest.mark.xfail(...))`, TID-51) already did this
//! correctly, so the two now share one fold: the same marker must mean the same thing whether it was
//! written above the test or attached while it ran.

#![cfg(unix)]

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
    candidates.into_iter().find(|p| {
        std::process::Command::new(p)
            .args(["-c", "import pytest"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    })
}

fn scratch(tag: &str) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "tiderace_t63_{tag}_{}_{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

const CORPUS: &str = r#"
import pytest


@pytest.mark.xfail
def test_bare_xfail_that_fails():
    raise AssertionError("the author expects this")


@pytest.mark.xfail(reason="documented")
def test_xfail_with_a_reason():
    raise AssertionError("also expected")


@pytest.mark.xfail(False, reason="condition is false, so this is an ordinary test")
def test_xfail_whose_condition_is_false():
    assert True


@pytest.mark.xfail
def test_xfail_that_unexpectedly_passes():
    assert True


@pytest.mark.xfail(strict=True)
def test_strict_xfail_that_unexpectedly_passes():
    assert True
"#;

#[test]
fn a_pytest_xfail_mark_maps_to_the_right_outcome() {
    let Some(python) = python_with_pytest() else {
        skip_live("no interpreter with pytest available");
        return;
    };
    let dir = scratch("xfail");
    std::fs::write(dir.join("test_xfail.py"), CORPUS).unwrap();

    let items = RegexCollector::new().collect(&dir).expect("collection");
    let results = SubprocessWorker::new(20_000, 1)
        .with_target(python, &shim(), &dir)
        .run(&items)
        .expect("batch runs");

    let outcome_of = |needle: &str| {
        results
            .iter()
            .find(|r| r.node_id.as_str().contains(needle))
            .unwrap_or_else(|| panic!("no result for {needle}"))
    };

    for (needle, expected) in [
        ("test_bare_xfail_that_fails", Outcome::XFail),
        ("test_xfail_with_a_reason", Outcome::XFail),
        // A false condition means the mark does not apply: an ordinary passing test.
        ("test_xfail_whose_condition_is_false", Outcome::Passed),
        ("test_xfail_that_unexpectedly_passes", Outcome::XPass),
        // strict=True turns an unexpected pass into a failure, as pytest does.
        (
            "test_strict_xfail_that_unexpectedly_passes",
            Outcome::Failed,
        ),
    ] {
        let r = outcome_of(needle);
        assert_eq!(r.outcome, expected, "TID-63: {needle} — {}", r.detail);
    }
    let _ = std::fs::remove_dir_all(&dir);
}
