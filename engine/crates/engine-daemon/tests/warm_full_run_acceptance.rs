//! TID-84 — `RunFull` serves a full parallel run from the daemon's warm image, and the image is
//! relaunched when the tree changes.
//!
//! The first `RunFull` launches the image (the import); the second forks from it and imports
//! nothing; a `.py` file added afterwards changes the tree stamp, so the third relaunches and sees
//! the new test. `Health` reports the image warm once it is held.

#![cfg(unix)]

use engine_core::domain::{Outcome, TestResult};
use engine_core::testing::{python, shim, skip_live, PythonNeeds};
use engine_daemon::{EngineHandler, RpcHandler, RpcRequest, RpcResponse};
use std::path::{Path, PathBuf};

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

fn run_full(handler: &mut EngineHandler) -> Vec<TestResult> {
    match handler.handle(RpcRequest::run_full_all()) {
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
    let Some(python) = python(PythonNeeds::Any) else {
        skip_live("no Python interpreter available");
        return;
    };
    let dir = write_corpus();
    let mut handler = EngineHandler::new(python, shim(), dir.clone());
    assert!(!warm(&mut handler), "nothing is warm before the first run");

    let first = run_full(&mut handler);
    assert_eq!(first.len(), 2, "{first:?}");
    let outcome = |rs: &[TestResult], leaf: &str| {
        rs.iter()
            .find(|r| r.node_id.as_str().ends_with(leaf))
            .map(|r| r.outcome)
    };
    assert_eq!(outcome(&first, "test_one"), Some(Outcome::Passed));
    assert_eq!(outcome(&first, "test_two"), Some(Outcome::Failed));
    assert_eq!(imports(&dir), 1, "the first run imported the suite");
    assert!(warm(&mut handler), "the image is held after the first run");
    assert!(
        first.iter().all(|r| r.worker.is_some() && r.unit.is_some()),
        "schedule stamps travel over the wire: {first:?}"
    );

    let second = run_full(&mut handler);
    assert_eq!(second.len(), 2);
    assert_eq!(outcome(&second, "test_two"), Some(Outcome::Failed));
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
    assert_eq!(outcome(&third, "test_three"), Some(Outcome::Passed));
    assert_eq!(imports(&dir), 2, "the relaunch imported the suite again");
    assert!(warm(&mut handler));

    let _ = std::fs::remove_dir_all(&dir);
}
