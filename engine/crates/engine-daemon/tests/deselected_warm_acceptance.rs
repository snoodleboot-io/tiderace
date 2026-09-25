//! TID-73 — a test the project's own `addopts` deselects does not run on every warm run.
//!
//! The Rust collector still collects such a node (it cannot read marks); the shim answers it with
//! an empty expansion, so it is absent from the tally as in pytest — and so it never had a record,
//! and the planner called it "never seen" on every warm run. On pirn-core that was 55 phantoms,
//! each a request, together forcing a wellspring launch to run nothing: 7.6s for a run that should
//! have been the hash pass alone. It is now recorded as `deselected`, with its module and the
//! config that deselected it as deps, so it is judged like any test and never served as one.

use engine_core::testing::skip_live;
use engine_daemon::EngineHandler;
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

fn python_with_tiderace() -> Option<String> {
    let venv = repo_root().join(".tiderace-fx-venv/bin/python");
    let mut cands: Vec<String> = Vec::new();
    if venv.exists() {
        cands.push(venv.to_string_lossy().into_owned());
    }
    cands.extend(["python3".to_string(), "python".to_string()]);
    cands.into_iter().find(|p| {
        std::process::Command::new(p)
            .args(["-c", "import tiderace"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    })
}

fn write_config(dir: &Path, addopts: &str) {
    std::fs::write(
        dir.join("pyproject.toml"),
        format!("[tool.pytest.ini_options]\nmarkers = [\"slow: slow\"]\naddopts = \"{addopts}\"\n"),
    )
    .unwrap();
}

#[test]
fn a_deselected_test_is_recorded_and_never_runs_again_until_the_config_changes() {
    let Some(python) = python_with_tiderace() else {
        skip_live("no interpreter that can import tiderace");
        return;
    };
    let dir = std::env::temp_dir().join(format!("tiderace_t73_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    write_config(&dir, "-m 'not slow'");
    std::fs::write(
        dir.join("test_marks.py"),
        "import tiderace\n\n\
         def test_fast():\n    assert True\n\n\
         @tiderace.mark.slow\n\
         def test_slow():\n    assert True\n",
    )
    .unwrap();

    let mut handler = EngineHandler::new(python, shim(), dir.clone());
    let cold = handler.run_full_parallel().expect("cold full run");
    assert_eq!(
        cold.len(),
        1,
        "the slow test is deselected, absent from the tally: {cold:?}"
    );

    let warm = handler.run_impacted().expect("warm run");
    assert_eq!(
        warm.ran, 0,
        "TID-73: nothing was edited, so nothing runs — the deselected node is not 'never seen'"
    );
    assert_eq!(
        warm.cached, 1,
        "one real result served; the deselected one is not a result"
    );
    assert_eq!(warm.results.len(), 1, "{:?}", warm.results);

    // The verdict depends on the config: drop the filter and the slow test is a real test again.
    write_config(&dir, "-ra");
    let warm = handler
        .run_impacted()
        .expect("warm run after a config change");
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        warm.ran, 1,
        "the config that deselected it changed, so it is re-evaluated — and now it runs: {:?}",
        warm.results
    );
    assert!(
        warm.results
            .iter()
            .any(|r| r.node_id.ends_with("test_slow")),
        "{:?}",
        warm.results
    );
}
