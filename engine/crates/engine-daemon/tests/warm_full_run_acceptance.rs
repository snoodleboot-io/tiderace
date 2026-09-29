//! TID-84 — `RunFull` serves a full parallel run from the daemon's warm image, and the image is
//! relaunched when the tree changes.
//!
//! The first `RunFull` launches the image (the import); the second forks from it and imports
//! nothing; a `.py` file added afterwards changes the tree stamp, so the third relaunches and sees
//! the new test. `Health` reports the image warm once it is held.

#![cfg(unix)]

use engine_core::testing::skip_live;
use engine_daemon::{EngineHandler, RpcHandler, RpcRequest, RpcResponse};
use std::path::{Path, PathBuf};

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

fn write_corpus() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("tiderace_t84_daemon_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("test_first.py"),
        "import os\n\
         with open(os.path.join(os.path.dirname(__file__), 'imports.log'), 'a') as f:\n    \
             f.write('imported\\n')\n\n\
         def test_one():\n    assert True\n\n\
         def test_two():\n    assert 1 == 2\n",
    )
    .unwrap();
    dir
}

fn imports(dir: &Path) -> usize {
    std::fs::read_to_string(dir.join("imports.log"))
        .map(|s| s.lines().count())
        .unwrap_or(0)
}

fn run_full(handler: &mut EngineHandler) -> Vec<engine_daemon::RpcFullResult> {
    match handler.handle(RpcRequest::RunFull) {
        RpcResponse::RanFull { results } => results,
        other => panic!("expected RanFull, got {other:?}"),
    }
}

fn warm(handler: &mut EngineHandler) -> bool {
    match handler.handle(RpcRequest::Health) {
        RpcResponse::Healthy { warm, .. } => warm,
        other => panic!("expected Healthy, got {other:?}"),
    }
}

#[test]
fn run_full_reuses_the_warm_image_until_the_tree_changes() {
    let Some(python) = any_python() else {
        skip_live("no Python interpreter available");
        return;
    };
    let dir = write_corpus();
    let mut handler = EngineHandler::new(python, shim(), dir.clone());
    assert!(!warm(&mut handler), "nothing is warm before the first run");

    let first = run_full(&mut handler);
    assert_eq!(first.len(), 2, "{first:?}");
    let outcome = |rs: &[engine_daemon::RpcFullResult], leaf: &str| {
        rs.iter()
            .find(|r| r.node_id.ends_with(leaf))
            .map(|r| r.outcome.clone())
    };
    assert_eq!(outcome(&first, "test_one"), Some("passed".into()));
    assert_eq!(outcome(&first, "test_two"), Some("failed".into()));
    assert_eq!(imports(&dir), 1, "the first run imported the suite");
    assert!(warm(&mut handler), "the image is held after the first run");
    assert!(
        first.iter().all(|r| r.worker.is_some() && r.unit.is_some()),
        "schedule stamps travel over the wire: {first:?}"
    );

    let second = run_full(&mut handler);
    assert_eq!(second.len(), 2);
    assert_eq!(outcome(&second, "test_two"), Some("failed".into()));
    assert_eq!(
        imports(&dir),
        1,
        "the second run forked from the image: no import"
    );

    // A new module: the tree stamp moves, the image is relaunched, and the run sees it.
    std::fs::write(
        dir.join("test_added.py"),
        "def test_three():\n    assert True\n",
    )
    .unwrap();
    let third = run_full(&mut handler);
    assert_eq!(third.len(), 3, "{third:?}");
    assert_eq!(outcome(&third, "test_three"), Some("passed".into()));
    assert_eq!(imports(&dir), 2, "the relaunch imported the suite again");
    assert!(warm(&mut handler));

    let _ = std::fs::remove_dir_all(&dir);
}
