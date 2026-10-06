//! TID-127 — the second `tiderace run` forks a recorded disturber from the start.
//!
//! A test that leaves a thread behind is caught on the in-process tier and re-run from the clean
//! image (TID-50) — an attempt and a re-run, every run, because the one-shot `run` wrote only
//! durations back and rediscovered the disturber each time. On the anyio suite that was fifteen
//! clean-room re-runs per run, and each attempt was a chance at the one-deadline stall. `run` now
//! records the disturbers it saw beside the durations, as a hint and never a verdict; the next run
//! forks them from the start and says so in its header.

#![cfg(unix)]

use engine_core::testing::{python, repo_root, PythonNeeds};
use std::path::PathBuf;
use std::process::Command;

/// The clean-room corpus's shape: the leak happens through a helper, because a test that starts a
/// thread in its own body is impure on sight and forks from the start, which is not this path.
fn write_project() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("tiderace_t127_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let tests = dir.join("tests");
    std::fs::create_dir_all(&tests).unwrap();
    std::fs::write(tests.join("__init__.py"), "").unwrap();
    std::fs::write(
        tests.join("leaky_lib.py"),
        "import threading\n\n_STARTED = False\n\n\ndef ensure_started():\n    global _STARTED\n    \
         if not _STARTED:\n        threading.Thread(target=threading.Event().wait, daemon=True).start()\n        \
         _STARTED = True\n",
    )
    .unwrap();
    std::fs::write(
        tests.join("test_leaky.py"),
        "from . import leaky_lib\n\n\ndef test_uses_the_library():\n    leaky_lib.ensure_started()\n\n\n\
         def test_ordinary():\n    assert True\n",
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
        .env("TIDERACE_NO_DAEMON", "1")
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
fn the_second_run_forks_a_recorded_disturber_from_the_start() {
    let Some(python) = python(PythonNeeds::Any) else {
        eprintln!("skipping: no Python interpreter available");
        return;
    };
    let dir = write_project();
    let tests = dir.join("tests");

    let first = run(&python, &tests);
    assert!(
        first.contains("re-running test_leaky.py::test_uses_the_library from a clean image"),
        "the first run discovers the disturber the expensive way: {first}"
    );
    let state: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(tests.join(".tiderace-state.json")).unwrap())
            .unwrap();
    assert_eq!(
        state["forced_fork"],
        serde_json::json!(["test_leaky.py::test_uses_the_library"]),
        "the disturber is recorded as a hint, not a verdict: {state}"
    );
    assert!(
        state["tests"].as_object().is_none_or(|t| t.is_empty()),
        "`run` leaves no TestRecord behind: {state}"
    );

    let second = run(&python, &tests);
    assert!(
        second.contains("1 forced-fork"),
        "the second run's header says the disturber is forked from the start: {second}"
    );
    assert!(
        !second.contains("re-running"),
        "nothing is rediscovered on the second run: {second}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
