//! TID-119 — a run whose state cannot be saved still reports its results.
//!
//! The daemon used to fail the whole run with an I/O error when the state file could not be
//! written — after the tests had run, with the answer in hand. The one policy now, shared with
//! `tiderace run`'s durations hint: say why the state was not saved and report the results; what
//! was not recorded runs again next time. Repeated work, never a stale verdict, never a green run
//! turned red by a full disk. The state path is a directory here, so every save fails with EISDIR
//! whatever the tree's permissions are (a root-run CI would ignore a read-only bit).

#![cfg(unix)]

use engine_core::domain::Outcome;
use engine_core::testing::{python, shim, skip_live, PythonNeeds};
use engine_daemon::EngineHandler;

#[test]
fn a_run_that_cannot_save_its_state_reports_its_results_and_goes_on() {
    let Some(python) = python(PythonNeeds::Any) else {
        skip_live("no Python interpreter available");
        return;
    };
    let dir = std::env::temp_dir().join(format!("tiderace_t119_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("test_plain.py"),
        "def test_a():\n    assert True\n\ndef test_b():\n    assert 1 + 1 == 2\n",
    )
    .unwrap();
    // The state file's name taken by a directory: unreadable as a state (loads empty) and
    // unwritable as a file (saves fail), from the first run on.
    std::fs::create_dir_all(dir.join(".tiderace-state.json")).unwrap();
    let mut handler = EngineHandler::new(python, shim(), dir.clone());

    let full = handler
        .run_full_parallel()
        .expect("the full run reports its results even though the state could not be saved");
    assert_eq!(full.len(), 2, "{full:?}");
    assert!(
        full.iter().all(|r| r.outcome == Outcome::Passed),
        "{full:?}"
    );

    // The impacted run has nothing recorded to lean on — it runs everything again, and again
    // cannot save — and still answers.
    let impacted = handler
        .run_impacted()
        .expect("the impacted run reports its results even though the state could not be saved");
    assert_eq!(
        impacted.ran, 2,
        "nothing was recorded, so everything runs again: {impacted:?}"
    );
    assert_eq!(impacted.cached, 0, "{impacted:?}");
    assert!(
        dir.join(".tiderace-state.json").is_dir(),
        "the directory standing in for the state file is left alone"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
