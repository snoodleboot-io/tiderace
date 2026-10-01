//! TID-104 — the sub-interpreter tier is sound: its results read as every other tier's, and a
//! test that blocks there ends the batch inside a budget rather than the run.
//!
//! Before this the tier read each response as one outcome, so a node `-k` deselected came back
//! as a pass and a parametrized node as a single result; and nothing ended a blocked test — a
//! sub-interpreter takes no signal and its watchdog thread cannot be a daemon — so click's suite
//! hung the Windows benchmark for an hour. Now each response is the shim's whole one, read by the
//! shared `results_for`; the shim's parent waits for each result at most the deadline plus the
//! lost-worker margin and reports what is outstanding by name; and the engine kills a pool whose
//! reply is overdue past the batch's budget.
//!
//! Needs an interpreter with `concurrent.interpreters` (3.14+): the fx venv on CI and here.

use engine_core::collection::{Collector, RegexCollector};
use engine_core::domain::Outcome;
use engine_core::exec::{SubInterpWorker, Worker};
use engine_core::testing::{python, scratch, shim, skip_live, PythonNeeds};
use std::time::Instant;

#[test]
fn responses_read_as_on_every_other_tier() {
    let Some(python) = python(PythonNeeds::SubInterpreters) else {
        skip_live("no interpreter with pytest and concurrent.interpreters");
        return;
    };
    let dir = scratch("shape");
    std::fs::write(
        dir.join("test_shape.py"),
        "import pytest\n\n\
         def test_plain():\n    assert True\n\n\
         @pytest.mark.parametrize(\"n\", [1, 2, 3])\n\
         def test_cases(n):\n    assert n > 0\n\n\
         @pytest.mark.skip(reason=\"never\")\n\
         def test_skipme():\n    assert False\n\n\
         def test_deselected_by_k():\n    assert False\n",
    )
    .unwrap();
    let items = RegexCollector::new().collect(&dir).expect("collection");
    assert_eq!(items.len(), 4);
    // `-k` travels to the pool as it does to every tier: through the environment.
    // SAFETY: this test binary's tests do not read the environment concurrently with this write.
    unsafe { std::env::set_var("TIDERACE_KEYWORD_EXPR", "not deselected") };
    let mut worker = SubInterpWorker::new(20_000)
        .with_target(python, &shim(), &dir)
        .with_pool_size(2);
    let results = worker.run(&items).expect("the batch runs");
    unsafe { std::env::remove_var("TIDERACE_KEYWORD_EXPR") };
    let mut ids: Vec<(String, String)> = results
        .iter()
        .map(|r| (r.node_id.to_string(), format!("{:?}", r.outcome)))
        .collect();
    ids.sort();
    let want: Vec<(String, String)> = [
        ("test_shape.py::test_cases[1]", Outcome::Passed),
        ("test_shape.py::test_cases[2]", Outcome::Passed),
        ("test_shape.py::test_cases[3]", Outcome::Passed),
        ("test_shape.py::test_plain", Outcome::Passed),
        ("test_shape.py::test_skipme", Outcome::Skipped),
    ]
    .into_iter()
    .map(|(id, oc)| (id.to_string(), format!("{oc:?}")))
    .collect();
    assert_eq!(
        ids, want,
        "TID-104: the cases expanded, the skip a skip, the deselected node absent — {results:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_test_that_blocks_in_a_sub_interpreter_ends_the_batch_inside_the_budget() {
    let Some(python) = python(PythonNeeds::SubInterpreters) else {
        skip_live("no interpreter with pytest and concurrent.interpreters");
        return;
    };
    let dir = scratch("block");
    std::fs::write(
        dir.join("test_block.py"),
        "import time\n\n\
         def test_a_quick():\n    assert True\n\n\
         def test_b_blocks_in_c():\n    time.sleep(120)\n\n\
         def test_c_quick():\n    assert True\n",
    )
    .unwrap();
    let items = RegexCollector::new().collect(&dir).expect("collection");
    const DEADLINE_MS: u64 = 1_500;
    let mut worker = SubInterpWorker::new(DEADLINE_MS)
        .with_target(python.clone(), &shim(), &dir)
        .with_pool_size(2);
    let started = Instant::now();
    let results = worker.run(&items).expect("the batch is reported, not lost");
    let elapsed = started.elapsed();
    assert!(
        elapsed.as_millis() < (DEADLINE_MS * 2 + 10_000 + 20_000) as u128,
        "ended by a budget, not by the sleep: {elapsed:?}"
    );
    let blocked = results
        .iter()
        .find(|r| r.node_id.as_str().ends_with("test_b_blocks_in_c"))
        .expect("the blocked test is reported");
    assert!(
        blocked.outcome == Outcome::Error && blocked.detail.contains("TID-104"),
        "the blocked test names what ended it: {:?} {}",
        blocked.outcome,
        blocked.detail
    );
    // The quick ones ran on the other interpreter, or are reported outstanding; either is an
    // answer, and the run went on.
    assert_eq!(results.len(), 3, "{results:?}");
    // The pool is gone; the next batch gets a fresh one.
    let again = worker
        .run(&items[..1])
        .expect("a fresh pool after a killed one");
    assert_eq!(again[0].outcome, Outcome::Passed, "{}", again[0].detail);
    let _ = std::fs::remove_dir_all(&dir);
}
