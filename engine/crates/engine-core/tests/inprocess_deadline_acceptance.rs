//! TID-93 — a test that blocks on the in-process ladder is ended by the deadline, and a worker
//! that cannot be interrupted is killed rather than waited on forever.
//!
//! The per-test deadline used to exist for forked children only. A test blocking in a lock on
//! the optimistic no-fork tier blocked its worker, and the engine's read on that worker had no
//! timeout: one such test on anyio stalled an eight-worker run for four minutes. Two layers now:
//! the shim arms `SIGALRM` around an in-process run, which ends any wait CPython lets a signal
//! interrupt; and the engine's read gives up a margin past the deadline, kills the worker, and
//! reports the batch — the in-flight node as the fault, the nodes after it as not run.

#![cfg(unix)]

use engine_core::collection::{Collector, RegexCollector};
use engine_core::domain::{Outcome, TestResult};
use engine_core::runner::{
    run_parallel, ForkOptions, Learned, RunPlan, SchedulerKind, WorkerCount, WorkerStrategy,
};
use engine_core::testing::{python, shim, skip_live, PythonNeeds};
use std::path::PathBuf;

fn get<'a>(results: &'a [TestResult], leaf: &str) -> &'a TestResult {
    results
        .iter()
        .find(|r| r.node_id.as_str().ends_with(leaf))
        .unwrap_or_else(|| panic!("{leaf} missing from {results:?}"))
}

fn write_corpus(tag: &str, body: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("tiderace_t93_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("test_block.py"), body).unwrap();
    dir
}

fn plan() -> RunPlan {
    RunPlan {
        fork: ForkOptions {
            shared_import: true,
            ..ForkOptions::default()
        },
        workers: WorkerCount::Default(1),
        strategy: WorkerStrategy::Fork,
        scheduler: SchedulerKind::Locality,
        deadline_ms: 2_000,
        ..RunPlan::default()
    }
}

/// A lock wait is interruptible: the shim's own deadline ends it, the worker survives, the next
/// test in the same file runs.
#[test]
fn a_blocking_lock_on_the_in_process_tier_is_a_timeout_and_the_worker_goes_on() {
    let Some(python) = python(PythonNeeds::Any) else {
        skip_live("no Python interpreter available");
        return;
    };
    let dir = write_corpus(
        "lock",
        "import threading\n\n\
         def test_blocks():\n    \
             lock = threading.Lock()\n    \
             lock.acquire()\n    \
             lock.acquire()  # never released: waits forever\n\n\
         def test_after():\n    assert True\n",
    );
    let items = RegexCollector::new().collect(&dir).expect("collection");
    let started = std::time::Instant::now();
    let results = run_parallel(&python, &shim(), &dir, items, &plan(), &Learned::default())
        .expect("the run ends");
    let elapsed = started.elapsed();
    let _ = std::fs::remove_dir_all(&dir);

    let blocked = get(&results, "test_blocks");
    assert_eq!(blocked.outcome, Outcome::Error, "{}", blocked.detail);
    assert!(
        blocked
            .detail
            .contains("timeout after 2s on the in-process tier"),
        "{}",
        blocked.detail
    );
    assert!(
        blocked.must_fork,
        "a test that overran in-process forks from now on"
    );
    assert_eq!(get(&results, "test_after").outcome, Outcome::Passed);
    assert!(
        elapsed < std::time::Duration::from_secs(9),
        "ended by the deadline, not the backstop: {elapsed:?}"
    );
}

/// A wait the signal cannot reach — `SIGALRM` blocked — leaves the worker silent: the engine's
/// read gives up a margin past the deadline, kills the worker, and reports the batch.
#[test]
fn a_worker_that_cannot_be_interrupted_is_killed_and_its_batch_reported() {
    let Some(python) = python(PythonNeeds::Any) else {
        skip_live("no Python interpreter available");
        return;
    };
    let dir = write_corpus(
        "masked",
        "import signal, threading\n\n\
         def test_blocks_with_alarm_masked():\n    \
             signal.pthread_sigmask(signal.SIG_BLOCK, {signal.SIGALRM})\n    \
             lock = threading.Lock()\n    \
             lock.acquire()\n    \
             lock.acquire()\n\n\
         def test_after():\n    assert True\n",
    );
    let items = RegexCollector::new().collect(&dir).expect("collection");
    let started = std::time::Instant::now();
    let results = run_parallel(&python, &shim(), &dir, items, &plan(), &Learned::default())
        .expect("the run ends");
    let elapsed = started.elapsed();
    let _ = std::fs::remove_dir_all(&dir);

    let blocked = get(&results, "test_blocks_with_alarm_masked");
    assert_eq!(blocked.outcome, Outcome::Error, "{}", blocked.detail);
    let after = get(&results, "test_after");
    assert_eq!(after.outcome, Outcome::Error, "{}", after.detail);
    assert!(
        after.detail.contains("not run: the worker was lost at"),
        "{}",
        after.detail
    );
    assert!(
        elapsed < std::time::Duration::from_secs(40),
        "the backstop fired: {elapsed:?}"
    );
}
