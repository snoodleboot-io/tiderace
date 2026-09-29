//! TID-89 — the daemon runs a test under the same deadline `tiderace run` does.
//!
//! The daemon handed its pool a 5,000 ms per-test deadline where the CLI's `RunPlan` uses the
//! engine's `DEFAULT_DEADLINE_MS` (60 s), so a class whose set-up ran two worker interpreters
//! — pirn-agents' cross-process replay — errored with `timeout` under the daemon only, and the
//! benchmark's warm rows carried "1 failing" for a pass. A test that takes six seconds now passes
//! through the daemon's full run and its warm `Run` alike.

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

#[test]
fn a_six_second_test_passes_through_the_daemon_as_it_does_through_the_cli() {
    let Some(python) = any_python() else {
        skip_live("no Python interpreter available");
        return;
    };
    let dir = std::env::temp_dir().join(format!("tiderace_t89_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("test_slow.py"),
        "import time\n\n\ndef test_slow_setup():\n    time.sleep(6)\n    assert True\n",
    )
    .unwrap();
    let mut handler = EngineHandler::new(python, shim(), dir.clone());

    let full = handler.run_full_parallel().expect("full run");
    assert_eq!(full.len(), 1, "{full:?}");
    assert_eq!(full[0].outcome, "passed", "full run: {full:?}");

    // The warm single-worker path (`Run`) launches its own wellspring with its own deadline.
    match handler.handle(RpcRequest::Run {
        node_ids: vec!["test_slow.py::test_slow_setup".to_string()],
    }) {
        RpcResponse::Ran { results } => {
            assert_eq!(results.len(), 1);
            assert_eq!(results[0].outcome, "passed", "warm Run: {results:?}");
        }
        other => panic!("expected Ran, got {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}
