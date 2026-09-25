//! TID-75 — the run after an edit imports only the modules it re-runs.
//!
//! After TID-73 the daemon's no-change run is 0.14s, and the run after a one-line edit to a module
//! one test depends on was 2.5s — nearly all of it a wellspring launch that pre-imported every
//! test module in the suite to run one. The impact run now hands the pool the modules its
//! `to_execute` set lives in, and the start-up imports those and the conftests.
//!
//! Observable because importing is a side effect: `test_b.py` writes a marker when imported. The
//! cold full run imports everything, so the marker appears; remove it, touch `test_a.py`, and the
//! impact run — which re-runs only `test_a`'s test — must not bring it back.

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
fn an_impact_run_imports_only_the_modules_it_reruns() {
    let Some(python) = any_python() else {
        skip_live("no Python interpreter available");
        return;
    };
    let dir = std::env::temp_dir().join(format!("tiderace_t75d_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let marker = dir.join("b_was_imported");
    std::fs::write(dir.join("test_a.py"), "def test_a():\n    assert True\n").unwrap();
    std::fs::write(
        dir.join("test_b.py"),
        format!(
            "open({:?}, 'w').close()\n\ndef test_b():\n    assert True\n",
            marker.to_string_lossy()
        ),
    )
    .unwrap();

    // Footprints come from coverage, and the impact run selects by footprint; the daemon's `main`
    // sets this for `run`, and the handler alone does not.
    // SAFETY: the only test in this binary; nothing else reads the environment concurrently.
    unsafe { std::env::set_var("TIDERACE_COVERAGE", "1") };
    let mut handler = EngineHandler::new(python, shim(), dir.clone());
    let cold = handler.run_full_parallel().expect("cold full run");
    assert_eq!(cold.len(), 2, "{cold:?}");
    assert!(marker.exists(), "a full run imports every module");

    std::fs::remove_file(&marker).unwrap();
    // Touch test_a.py: its own test depends on it (every test's footprint includes its file), and
    // nothing in test_b.py does.
    let a = dir.join("test_a.py");
    let mut src = std::fs::read_to_string(&a).unwrap();
    src.push_str("\n# edit\n");
    std::fs::write(&a, src).unwrap();

    let warm = handler.run_impacted().expect("impact run");
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(warm.ran, 1, "only test_a re-runs: {:?}", warm.results);
    assert!(
        !marker.exists(),
        "TID-75: the impact run re-ran test_a's test, so test_b.py must not have been imported — \
         the start-up pre-imported the whole suite for a one-module run"
    );
}
