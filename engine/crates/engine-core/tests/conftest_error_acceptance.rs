//! TID-72 — a conftest that fails to import errors every test under it.
//!
//! It used to cost only its fixtures: the tests below ran anyway and mostly passed — 508 of 511 on
//! fx_corpus with a conftest that raised on import — while the few that needed one of its fixtures
//! failed naming the fixture rather than the cause. pytest stops at collection with the conftest's
//! own error and runs nothing. The per-test equivalent of that verdict is every test under the
//! conftest reporting `error` with the conftest's traceback, which is what happens now — the way a
//! conftest that *skips* at import already skips everything under it (TID-48). Tests elsewhere are
//! untouched.

use engine_core::collection::{Collector, RegexCollector};
use engine_core::domain::Outcome;
use engine_core::exec::{SubprocessWorker, Worker};
use engine_core::testing::{python, scratch, shim, skip_live, PythonNeeds};

#[test]
fn every_test_under_a_conftest_that_fails_to_import_is_an_error() {
    let Some(python) = python(PythonNeeds::Any) else {
        skip_live("no Python interpreter available");
        return;
    };
    let dir = scratch("broken");
    std::fs::create_dir_all(dir.join("broken")).unwrap();
    std::fs::write(dir.join("test_ok.py"), "def test_ok():\n    assert True\n").unwrap();
    // The conftest needs nothing the tests need; they would pass without it. That is the point.
    std::fs::write(
        dir.join("broken/conftest.py"),
        "raise RuntimeError('no such backend: the setup for this directory is broken')\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("broken/test_under.py"),
        "def test_a():\n    assert True\n\ndef test_b():\n    assert True\n",
    )
    .unwrap();

    let items = RegexCollector::new().collect(&dir).expect("collection");
    assert_eq!(items.len(), 3, "{items:?}");
    let results = SubprocessWorker::new(20_000, 1)
        .with_target(python, &shim(), &dir)
        .run(&items)
        .expect("batch runs");

    let ok = results
        .iter()
        .find(|r| r.node_id.as_str().ends_with("test_ok"))
        .expect("test_ok reports");
    assert_eq!(
        ok.outcome,
        Outcome::Passed,
        "a test elsewhere is untouched: {}",
        ok.detail
    );

    for name in ["test_a", "test_b"] {
        let r = results
            .iter()
            .find(|r| r.node_id.as_str().ends_with(name))
            .unwrap_or_else(|| panic!("{name} reports: {results:?}"));
        assert_eq!(
            r.outcome,
            Outcome::Error,
            "TID-72: {name} sits under a conftest that did not import — it would have passed \
             without it, which is exactly the wrong answer: {}",
            r.detail
        );
        assert!(
            r.detail.contains("conftest") && r.detail.contains("no such backend"),
            "the detail is the conftest's own failure, not a fixture's: {}",
            r.detail
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
