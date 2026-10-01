//! TID-98 — the Windows path of the no-fork deadline, forced on any platform.
//!
//! `TIDERACE_DEADLINE_WATCHDOG=1` makes the shim arm its watchdog thread instead of `setitimer`,
//! which is what it does where `setitimer` does not exist. The watchdog ends a busy test at the
//! next bytecode boundary; a test blocked in a C call it cannot reach, and that one the engine's
//! read budget ends — the worker killed, the node reported lost, the rest of the batch not run,
//! and a fresh worker for the next batch. This file holds both, so the Linux job proves the
//! Windows behaviour without a Windows machine.

use engine_core::collection::{Collector, RegexCollector};
use engine_core::domain::Outcome;
use engine_core::exec::{SubprocessWorker, Worker};
use engine_core::testing::{python, scratch, shim, skip_live, PythonNeeds};
use std::time::Instant;

const DEADLINE_MS: u64 = 1_500;

fn force_watchdog() {
    // SAFETY: this binary's tests all want the same value; nothing reads the environment
    // concurrently with the write.
    unsafe { std::env::set_var("TIDERACE_DEADLINE_WATCHDOG", "1") };
}

#[test]
fn under_the_watchdog_a_busy_test_is_ended_and_the_worker_survives() {
    let Some(python) = python(PythonNeeds::Any) else {
        skip_live("no Python interpreter available");
        return;
    };
    force_watchdog();
    let dir = scratch("wd_busy");
    std::fs::write(
        dir.join("test_busy.py"),
        "def test_a_spins_forever():\n    while True:\n        pass\n\n\n\
         def test_b_after_it():\n    assert True\n",
    )
    .unwrap();
    let items = RegexCollector::new().collect(&dir).expect("collection");
    let mut worker = SubprocessWorker::new(DEADLINE_MS, 1).with_target(python, &shim(), &dir);
    let started = Instant::now();
    let results = worker.run(&items).expect("the batch is reported");
    assert!(started.elapsed().as_millis() < (DEADLINE_MS + 10_000 + 20_000) as u128);
    let spins = &results[0];
    assert!(
        spins.outcome == Outcome::Error && spins.detail.contains("timeout after"),
        "the watchdog ends a busy test: {:?} {}",
        spins.outcome,
        spins.detail
    );
    assert_eq!(results[1].outcome, Outcome::Passed, "{}", results[1].detail);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn under_the_watchdog_a_test_blocked_in_c_is_ended_by_the_engine() {
    let Some(python) = python(PythonNeeds::Any) else {
        skip_live("no Python interpreter available");
        return;
    };
    force_watchdog();
    let dir = scratch("wd_sleep");
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
        .expect("a lost worker is reported per node");
    let elapsed = started.elapsed();
    assert!(
        elapsed.as_millis() < (DEADLINE_MS + 10_000 + 20_000) as u128,
        "ended by the budget, not by the sleep: {elapsed:?}"
    );
    assert!(
        elapsed.as_millis() >= DEADLINE_MS as u128,
        "the budget is not shorter than the deadline: {elapsed:?}"
    );
    let sleeper = &results[0];
    assert!(
        sleeper.outcome == Outcome::Error && sleeper.detail.contains("TID-98"),
        "the watchdog cannot reach a C call; the engine's budget ends it: {:?} {}",
        sleeper.outcome,
        sleeper.detail
    );
    assert!(
        results[1].outcome == Outcome::Error && results[1].detail.contains("not run"),
        "{:?} {}",
        results[1].outcome,
        results[1].detail
    );
    let again = worker
        .run(&items[1..])
        .expect("a fresh worker after a lost one");
    assert_eq!(again[0].outcome, Outcome::Passed, "{}", again[0].detail);
    let _ = std::fs::remove_dir_all(&dir);
}
