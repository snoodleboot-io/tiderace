//! TID-92 — a filtered run through the daemon leaves the persisted verdicts alone.
//!
//! `persist_results` records a candidate that ran and produced nothing as `deselected` (TID-73:
//! the project's own `addopts`). Under a `-k` that matches nothing every node produces nothing,
//! and after one such `RunFull` the next impacted run reported 522 of pirn-core's 5,602 nodes.
//! A filtered run learns nothing about deselection; the nodes it did run are recorded as usual.

#![cfg(unix)]

use engine_core::testing::skip_live;
use engine_daemon::{EngineHandler, RpcHandler, RpcRequest, RpcResponse};
use std::path::PathBuf;

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
    let Some(python) = any_python() else {
        skip_live("no Python interpreter available");
        return;
    };
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
            assert!(results[0].node_id.ends_with("test_three"));
        }
        other => panic!("expected RanFull, got {other:?}"),
    }
    assert_eq!(deselected_records(&dir), 0);

    // The impacted run afterwards still serves all three from cache.
    let warm = handler.run_impacted().expect("impacted run");
    assert_eq!(warm.results.len(), 3, "{:?}", warm.results);
    assert_eq!(warm.ran, 0);
    assert_eq!(warm.cached, 3);
    let _ = std::fs::remove_dir_all(&dir);
}
