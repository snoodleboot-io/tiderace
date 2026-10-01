//! TID-98 — the per-test deadline holds on the no-fork tier on every platform, Windows included.
//!
//! Two layers, each proven by the test that only it can end:
//!
//! * a **busy** test (`while True: pass`) is ended by the shim — `setitimer`'s signal on Unix, the
//!   watchdog thread's `PyThreadState_SetAsyncExc` elsewhere — reported as an error with the timeout
//!   message, and the worker lives on to run the next test;
//! * a test **blocked in a C call** (`time.sleep`) is beyond the watchdog on Windows (the async
//!   exception lands at the next bytecode boundary, which a sleep never reaches) and is ended by
//!   the engine's read budget: the worker is killed and reported lost, this node names the fault,
//!   the rest of its batch is reported as not run. On Unix the signal interrupts the sleep too, so
//!   there it is the first kind. Either way the run ends inside the budget.
//!
//! Runs on the Windows CI job (a Python is provisioned; no fork needed) as well as on Linux.

use engine_core::collection::{Collector, RegexCollector};
use engine_core::domain::Outcome;
use engine_core::exec::{SubprocessWorker, Worker};
use engine_core::testing::{python, scratch, shim, skip_live, PythonNeeds};
use std::time::Instant;

const DEADLINE_MS: u64 = 1_500;

#[test]
fn a_busy_test_is_ended_by_the_shim_and_the_worker_survives() {
    let Some(python) = python(PythonNeeds::Any) else {
        skip_live("no Python interpreter available");
        return;
    };
    let dir = scratch("busy");
    std::fs::write(
        dir.join("test_busy.py"),
        "def test_a_spins_forever():\n    while True:\n        pass\n\n\n\
         def test_b_after_it():\n    assert True\n",
    )
    .unwrap();
    let items = RegexCollector::new().collect(&dir).expect("collection");
    assert_eq!(items.len(), 2);
    let mut worker = SubprocessWorker::new(DEADLINE_MS, 1).with_target(python, &shim(), &dir);
    let started = Instant::now();
    let results = worker.run(&items).expect("the batch is reported");
    let elapsed = started.elapsed();
    assert!(
        elapsed.as_millis() < (DEADLINE_MS + 10_000 + 20_000) as u128,
        "the run must end inside the deadline and the engine's margin: {elapsed:?}"
    );
    let spins = results
        .iter()
        .find(|r| r.node_id.as_str().ends_with("test_a_spins_forever"))
        .expect("the busy test is reported");
    assert!(
        spins.outcome == Outcome::Error && spins.detail.contains("timeout after"),
        "TID-98: the busy test is ended by the shim's deadline: {:?} {}",
        spins.outcome,
        spins.detail
    );
    let after = results
        .iter()
        .find(|r| r.node_id.as_str().ends_with("test_b_after_it"))
        .expect("the next test is reported");
    assert_eq!(
        after.outcome,
        Outcome::Passed,
        "the worker survives a timeout it delivered itself: {}",
        after.detail
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_test_blocked_in_c_is_ended_inside_the_engines_budget() {
    let Some(python) = python(PythonNeeds::Any) else {
        skip_live("no Python interpreter available");
        return;
    };
    let dir = scratch("sleep");
    std::fs::write(
        dir.join("test_sleep.py"),
        "import time\n\n\ndef test_a_sleeps_past_everything():\n    time.sleep(120)\n\n\n\
         def test_b_after_it():\n    assert True\n",
    )
    .unwrap();
    let items = RegexCollector::new().collect(&dir).expect("collection");
    let mut worker = SubprocessWorker::new(DEADLINE_MS, 1).with_target(python, &shim(), &dir);
    let started = Instant::now();
    let results = worker
        .run(&items)
        .expect("a lost worker is reported per node, not as a failed batch");
    let elapsed = started.elapsed();
    assert!(
        elapsed.as_millis() < (DEADLINE_MS + 10_000 + 20_000) as u128,
        "the run must end inside the deadline and the engine's margin, not after the sleep: \
         {elapsed:?}"
    );
    let sleeper = results
        .iter()
        .find(|r| {
            r.node_id
                .as_str()
                .ends_with("test_a_sleeps_past_everything")
        })
        .expect("the sleeping test is reported");
    let by_shim = sleeper.outcome == Outcome::Error && sleeper.detail.contains("timeout after");
    let by_engine = sleeper.outcome == Outcome::Error && sleeper.detail.contains("TID-98");
    assert!(
        by_shim || by_engine,
        "TID-98: ended by the shim's signal or by the engine's budget: {:?} {}",
        sleeper.outcome,
        sleeper.detail
    );
    let after = results
        .iter()
        .find(|r| r.node_id.as_str().ends_with("test_b_after_it"))
        .expect("the next test is reported either way");
    if by_engine {
        assert!(
            after.outcome == Outcome::Error && after.detail.contains("not run"),
            "the rest of a lost worker's batch is reported as not run: {:?} {}",
            after.outcome,
            after.detail
        );
    } else {
        assert_eq!(after.outcome, Outcome::Passed, "{}", after.detail);
    }
    // And the worker is replaced: the next batch runs.
    let again = worker
        .run(&items[1..])
        .expect("a fresh worker after a lost one");
    assert_eq!(again.len(), 1);
    assert_eq!(again[0].outcome, Outcome::Passed, "{}", again[0].detail);
    let _ = std::fs::remove_dir_all(&dir);
}
