//! TID-17 — every isolation tier is reachable, and they agree.
//!
//! `engine-cli` had no flags, so `run` always used one tier and one scheduler while the engine
//! shipped three and two. Nothing outside the daemon could select among them, which meant the
//! sub-interpreter tier and the locality scheduler were never exercised through the front end and
//! every measurement taken through it described one configuration while being reported unqualified.
//!
//! Two properties are pinned here:
//!   * **reachability** — each tier available on this platform actually runs a corpus;
//!   * **agreement** — they produce the *same outcomes*, which is what makes the choice a
//!     performance decision rather than a correctness one. A tier that quietly reported different
//!     results would be far worse than one that could not be selected.
//!
//! Stdlib-only corpus, so any interpreter on `PATH` runs it.

use engine_core::collection::{Collector, RegexCollector};
use engine_core::domain::{Outcome, TestResult};
use engine_core::runner::{run_parallel, RunPlan, SchedulerKind, WorkerStrategy};
use engine_core::testing::{python, shim, skip_live, PythonNeeds};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

/// Spans outcome types and both styles, across two modules so the scheduler has something to pack.
const MODULE_A: &str = "\
def test_a_pass():
    assert 1 + 1 == 2


def test_a_fail():
    assert 1 == 2


def test_a_error():
    raise RuntimeError(\"boom\")


class TestGroup:
    def test_method_pass(self):
        assert \"x\".upper() == \"X\"
";

const MODULE_B: &str = "\
import unittest


def test_b_pass():
    assert sorted([3, 1, 2]) == [1, 2, 3]


def test_b_skip():
    raise unittest.SkipTest(\"not applicable\")


class TestOther:
    def test_method_fail(self):
        assert []
";

fn write_corpus(tag: &str) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "tiderace_tiers_{tag}_{}_{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("test_alpha.py"), MODULE_A).unwrap();
    std::fs::write(dir.join("test_beta.py"), MODULE_B).unwrap();
    dir
}

/// `(node_id, outcome)` sorted by node id — `Outcome` isn't `Ord`, but the node id is a total key.
fn fingerprint(results: &[TestResult]) -> Vec<(String, Outcome)> {
    let mut v: Vec<(String, Outcome)> = results
        .iter()
        .map(|r| (r.node_id.to_string(), r.outcome))
        .collect();
    v.sort_by(|a, b| a.0.cmp(&b.0));
    v
}

#[test]
fn every_available_tier_runs_the_corpus_and_agrees() {
    let Some(python) = python(PythonNeeds::Any) else {
        skip_live("no Python interpreter available");
        return;
    };
    let dir = write_corpus("agree");
    let items = RegexCollector::new().collect(&dir).expect("collection");
    assert_eq!(items.len(), 7, "7 tests across the two modules");

    let mut baseline: Option<(WorkerStrategy, Vec<(String, Outcome)>)> = None;
    let mut ran = 0;

    for strategy in [
        WorkerStrategy::Fork,
        WorkerStrategy::Subprocess,
        WorkerStrategy::SubInterp,
    ] {
        if !strategy.is_available() {
            continue;
        }
        let plan = RunPlan {
            strategy,
            workers: 2,
            ..RunPlan::default()
        };
        let results = run_parallel(&python, &shim(), &dir, items.clone(), &plan)
            .unwrap_or_else(|e| panic!("TID-17: the {strategy} tier must run the corpus: {e}"));
        assert_eq!(
            results.len(),
            items.len(),
            "the {strategy} tier must return one result per test"
        );
        ran += 1;

        let fp = fingerprint(&results);
        match &baseline {
            None => baseline = Some((strategy, fp)),
            Some((first, expected)) => assert_eq!(
                &fp,
                expected,
                "TID-17: the {strategy} tier disagreed with {first} — selecting a tier must be a \
                 performance decision, not a correctness one; details: {}",
                results
                    .iter()
                    .map(|r| format!(
                        "{} => {}",
                        r.node_id,
                        r.detail.chars().take(240).collect::<String>()
                    ))
                    .collect::<Vec<_>>()
                    .join(" | ")
            ),
        }
    }

    assert!(ran >= 2, "at least fork/subprocess must be reachable here");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The scheduler must not change results either — it only decides who runs what, in what order.
#[test]
fn both_schedulers_agree_on_the_same_corpus() {
    let Some(python) = python(PythonNeeds::Any) else {
        skip_live("no Python interpreter available");
        return;
    };
    let dir = write_corpus("sched");
    let items = RegexCollector::new().collect(&dir).expect("collection");

    let run = |scheduler: SchedulerKind| {
        let plan = RunPlan {
            scheduler,
            workers: 3,
            ..RunPlan::default()
        };
        let results = run_parallel(&python, &shim(), &dir, items.clone(), &plan)
            .unwrap_or_else(|e| panic!("the {scheduler} scheduler must run the corpus: {e}"));
        fingerprint(&results)
    };

    assert_eq!(
        run(SchedulerKind::Locality),
        run(SchedulerKind::RoundRobin),
        "packing must not change outcomes"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Worker count is a throughput knob, not a semantic one — and 1 worker must still run everything.
#[test]
fn worker_count_does_not_change_results() {
    let Some(python) = python(PythonNeeds::Any) else {
        skip_live("no Python interpreter available");
        return;
    };
    let dir = write_corpus("workers");
    let items = RegexCollector::new().collect(&dir).expect("collection");

    let run = |workers: usize| {
        let plan = RunPlan {
            workers,
            ..RunPlan::default()
        };
        fingerprint(
            &run_parallel(&python, &shim(), &dir, items.clone(), &plan)
                .unwrap_or_else(|e| panic!("{workers} worker(s) must run the corpus: {e}")),
        )
    };

    let sequential = run(1);
    assert_eq!(
        sequential.len(),
        items.len(),
        "1 worker still runs all tests"
    );
    assert_eq!(sequential, run(4), "worker count must not change outcomes");
    let _ = std::fs::remove_dir_all(&dir);
}
