//! TID-92 — a filtered run through the daemon leaves the persisted verdicts alone.
//!
//! `persist_results` records a candidate that ran and produced nothing as `deselected` (TID-73:
//! the project's own `addopts`). Under a `-k` that matches nothing every node produces nothing,
//! and after one such `RunFull` the next impacted run reported 522 of pirn-core's 5,602 nodes.
//! A filtered run learns nothing about deselection; the nodes it did run are recorded as usual.

#![cfg(unix)]

use engine_core::domain::Outcome;
use engine_core::testing::{python, shim, skip_live, PythonNeeds};
use engine_daemon::{EngineHandler, RpcHandler, RpcRequest, RpcResponse};

fn deselected_records(dir: &std::path::Path) -> usize {
    let state: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(dir.join(".tiderace-state.json")).unwrap_or_default(),
    )
    .unwrap_or_default();
    state["tests"]
        .as_object()
        .map(|t| t.values().filter(|r| r["outcome"] == "deselected").count())
        .unwrap_or(0)
}

#[test]
fn a_keyword_run_that_selects_nothing_does_not_turn_the_suite_deselected() {
    let Some(python) = python(PythonNeeds::Any) else {
        skip_live("no Python interpreter available");
        return;
    };
    // Footprints are what an impacted run reads to know an edit matters; `tiderace-daemon run`
    // sets this itself, a handler built here does not.
    // SAFETY: this binary holds one test; nothing else reads the environment concurrently.
    unsafe { std::env::set_var("TIDERACE_COVERAGE", "1") };
    let dir = std::env::temp_dir().join(format!("tiderace_t92_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("test_a.py"),
        "def test_one():\n    assert True\n\n\ndef test_two():\n    assert True\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("test_b.py"),
        "def test_three():\n    assert True\n",
    )
    .unwrap();
    let mut handler = EngineHandler::new(python, shim(), dir.clone());

    // The baseline: every test recorded, none deselected.
    let full = handler.run_full_parallel().expect("full run");
    assert_eq!(full.len(), 3, "{full:?}");
    assert_eq!(deselected_records(&dir), 0);

    // A `-k` that matches nothing: correct answer, and no verdict learned from it.
    match handler.handle(RpcRequest::RunFull {
        keyword: Some("nomatch_zzz".into()),
        marker: None,
        strict_markers: false,
    }) {
        RpcResponse::RanFull { results } => assert!(results.is_empty(), "{results:?}"),
        other => panic!("expected RanFull, got {other:?}"),
    }
    assert_eq!(
        deselected_records(&dir),
        0,
        "a filtered run must not record the unselected nodes as deselected"
    );

    // A `-k` that selects one: that one is recorded, the other two untouched.
    match handler.handle(RpcRequest::RunFull {
        keyword: Some("test_three".into()),
        marker: None,
        strict_markers: false,
    }) {
        RpcResponse::RanFull { results } => {
            assert_eq!(results.len(), 1, "{results:?}");
            assert!(results[0].node_id.as_str().ends_with("test_three"));
        }
        other => panic!("expected RanFull, got {other:?}"),
    }
    assert_eq!(deselected_records(&dir), 0);

    // The impacted run afterwards still serves all three from cache.
    let warm = handler.run_impacted().expect("impacted run");
    assert_eq!(warm.results.len(), 3, "{:?}", warm.results);
    assert_eq!(warm.ran, 0);
    assert_eq!(warm.cached, 3);

    // An edit, then a `-k` that does not select the edited test: the filtered run must not
    // re-baseline the edited file's hash, or the impacted run after it would serve test_three's
    // stale pass from cache (TID-94). It re-runs it and reports the new failure.
    std::fs::write(
        dir.join("test_b.py"),
        "def test_three():\n    assert False\n",
    )
    .unwrap();
    match handler.handle(RpcRequest::RunFull {
        keyword: Some("test_one".into()),
        marker: None,
        strict_markers: false,
    }) {
        RpcResponse::RanFull { results } => assert_eq!(results.len(), 1, "{results:?}"),
        other => panic!("expected RanFull, got {other:?}"),
    }
    let after_edit = handler.run_impacted().expect("impacted run after the edit");
    assert_eq!(after_edit.ran, 1, "{:?}", after_edit.results);
    let three = after_edit
        .results
        .iter()
        .find(|r| r.node_id.as_str().ends_with("test_three"))
        .expect("test_three reported");
    assert_eq!(three.outcome, Outcome::Failed, "{:?}", after_edit.results);
    let _ = std::fs::remove_dir_all(&dir);
}
