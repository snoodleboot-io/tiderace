//! TID-71 — a warm run with nothing edited re-runs nothing, parametrized tests included.
//!
//! A parametrized node is *collected* as `test_p` and *recorded* per case as `test_p[a]`, `test_p[b]`
//! (TID-25). The impact planner judged each collected candidate by its own key, so such a node had
//! no record, was "never seen", and re-ran on every warm run forever — one node on fx_corpus, and
//! the tally did not add up (`1 ran, 508 cached, 511 total`). It is now judged by its cases and the
//! cases are what is served from cache.

use engine_core::testing::skip_live;
use engine_daemon::EngineHandler;
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

/// A Python that can `import tiderace` — the fx venv, or whatever `PYTHONPATH` makes work.
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

#[test]
fn a_warm_run_with_nothing_edited_reruns_nothing() {
    let Some(python) = python_with_tiderace() else {
        skip_live("no interpreter that can import tiderace");
        return;
    };
    let dir = std::env::temp_dir().join(format!("tiderace_t71_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("test_warm.py"),
        "import tiderace\n\n\
         def test_plain():\n    assert True\n\n\
         @tiderace.cases([1, 2, 3])\n\
         def test_cases(n):\n    assert n > 0\n",
    )
    .unwrap();

    let mut handler = EngineHandler::new(python, shim(), dir.clone());
    let cold = handler.run_full_parallel().expect("cold full run");
    assert_eq!(cold.len(), 4, "one plain test and three cases: {cold:?}");

    let warm = handler.run_impacted().expect("warm run");
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        warm.ran, 0,
        "TID-71: nothing was edited, so nothing runs — the parametrized node included"
    );
    assert_eq!(
        warm.cached,
        4,
        "and every recorded id is served, cases under their own ids: {:?}",
        warm.results
            .iter()
            .map(|r| r.node_id.as_str())
            .collect::<Vec<_>>()
    );
    assert_eq!(warm.results.len(), 4);
}
