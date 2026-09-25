//! TID-62 — the second `tiderace run` is warm: it knows what every test cost last time, and says so.
//!
//! The queue (TID-52) decided *where* work goes; recorded durations decide *what order*. On a cold
//! run every collected item weighs 1, and on a suite whose per-test cost spans four orders of
//! magnitude that order is barely better than none. The first run records each node's wall clock;
//! the second run's scheduler weights units by it, and the run header states that it did — a pasted
//! number is uninterpretable without knowing whether the run was warm.
//!
//! What `run` writes is durations and nothing else. It must not leave a `TestRecord` behind: the
//! impact planner reads a record with no changed deps as "up to date", so a record written only to
//! carry a duration would turn the daemon's next impact-aware run into a stale pass.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repo root")
}

fn any_python() -> Option<String> {
    let venv = repo_root().join(".tiderace-fx-venv/bin/python");
    if venv.exists() {
        return Some(venv.to_string_lossy().into_owned());
    }
    ["python3", "python"]
        .into_iter()
        .find(|cand| {
            Command::new(cand)
                .arg("--version")
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        })
        .map(str::to_string)
}

fn write_project() -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "tiderace_t62_{}_{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let tests = dir.join("tests");
    std::fs::create_dir_all(&tests).unwrap();
    std::fs::write(
        tests.join("test_quick.py"),
        "def test_one():\n    assert True\n\ndef test_two():\n    assert True\n",
    )
    .unwrap();
    std::fs::write(
        tests.join("test_slow.py"),
        "import time\n\ndef test_sleeps():\n    time.sleep(0.2)\n",
    )
    .unwrap();
    dir
}

fn run(python: &str, tests: &PathBuf) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_tiderace"))
        .arg("run")
        .arg("-q")
        .arg(tests)
        .env("TIDERACE_PYTHON", python)
        .env("TIDERACE_SHIM", repo_root().join("engine/py-shim/shim.py"))
        .output()
        .expect("the CLI runs");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn the_second_run_is_warm_and_says_so() {
    let Some(python) = any_python() else {
        eprintln!("skipping: no Python interpreter available");
        return;
    };
    let dir = write_project();
    let tests = dir.join("tests");

    let first = run(&python, &tests);
    assert!(
        !first.contains("durations"),
        "the first run on a fresh tree has nothing to be warm from: {first}"
    );

    let state_path = tests.join(".tiderace-state.json");
    let state: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&state_path).expect("run wrote the state file"),
    )
    .expect("valid JSON");
    let durations = state["durations"]
        .as_object()
        .expect("TID-62: the first run recorded a duration per node");
    assert_eq!(durations.len(), 3, "one per reported node: {durations:?}");
    assert!(
        durations["test_slow.py::test_sleeps"].as_u64().unwrap() >= 200,
        "the sleeping test's recorded cost includes its sleep: {durations:?}"
    );
    assert!(
        state
            .get("tests")
            .is_none_or(|t| t.as_object().is_none_or(|o| o.is_empty())),
        "a run writes durations and NOTHING else — a TestRecord here would be read by the impact \
         planner as an up-to-date verdict: {state}"
    );

    let second = run(&python, &tests);
    assert!(
        second.contains("learned=3 durations"),
        "the second run's header says it weighted the schedule by what the first one learned: {second}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
