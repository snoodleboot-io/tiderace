//! Fixtures under the real engine — the two scenarios that need a live interpreter.
//!
//! The Phase 3 acceptance suite once also exercised a Rust `FixtureGraph` over hand-built inputs;
//! fixture resolution is the shim's (`tiderace_shim/fixtures.py`, ADR-E014), so that graph and
//! its scenarios were retired with it (TID-110). What is left is what no unit test can say:
//!
//!   8 reinit_after_fork .......... `forked_child_gets_fresh_sqlite_connection`
//!   9 fallback parity ............ `subprocess_worker_outcomes_and_teardown_match_fork`
//!
//! Both drive the engine against `fx_corpus` through the real Wellspring/Worker on the **live
//! venv** — the python/sqlite boundary is **NEVER mocked** (BINDING). They skip cleanly (an
//! early return with a SKIP note) when the fx venv is absent, exactly like `differential.rs`.

mod fx_support;

use engine_core::collection::{Collector, RegexCollector};
use engine_core::domain::Outcome;
use engine_core::exec::{ForkWorker, SubprocessWorker, Worker};
use engine_core::testing::{python, shim, skip_live, PythonNeeds};
use fx_support::{fx_corpus_root, run_pytest_oracle};

// Scenario 8 (PLAN §7 + §8 boundary 2: reinit_after_fork) — LIVE fork + sqlite.
// =====================================================================
//
// Given the sqlite in-memory connection fixture (reinit_after_fork__db_conn), Then
// each forked child opens a FRESH connection (distinct identity) and the parent's
// connection is NEVER used in-child — the load-bearing ADR-E003 safety claim. We
// drive the real corpus through the real ForkWorker; sqlite is NEVER mocked.
#[test]
fn forked_child_gets_fresh_sqlite_connection() {
    let Some(python) = python(PythonNeeds::FxVenv).map(std::path::PathBuf::from) else {
        skip_live("`.tiderace-fx-venv` not found — run the Phase-3 Lane-0 env gate first");
        return;
    };
    let _live = fx_support::live_guard(); // serialize corpus-launching scenarios (env isolation)

    // The two sqlite-resource tests must both pass under fork (each child reopens the
    // connection and sees the seeded rows): if a child wrongly inherited the parent's
    // post-fork-corrupted handle, the query would error.
    let results = run_engine_on_corpus(&python, /* fork */ true);

    let sqlite: Vec<&engine_core::domain::TestResult> = results
        .iter()
        .filter(|r| r.node_id.as_str().contains("test_sqlite"))
        .collect();
    assert_eq!(
        sqlite.len(),
        2,
        "both sqlite-resource tests must be collected + executed; got {}",
        sqlite.len()
    );
    for r in &sqlite {
        assert_eq!(
            r.outcome,
            Outcome::Passed,
            "sqlite test {} must pass — a fresh in-child connection sees the seed data; \
             a corrupted inherited handle would error. detail: {}",
            r.node_id,
            r.detail
        );
    }

    // The probe records the resource fixture's body ran once per test (a fresh
    // connection acquired in each child), never shared across children.
    let oracle = run_pytest_oracle(&python);
    assert_eq!(
        oracle.counts.get("reinit_after_fork__db_conn"),
        Some(&2),
        "the non-fork-safe connection fixture must be (re)built once per child"
    );
}

// =====================================================================
// Scenario 9 (fallback parity) — LIVE, both workers.
// =====================================================================
//
// Given --no-fork (SubprocessWorker), Then outcomes + teardown ordering are
// IDENTICAL to the fork path on the same corpus (CONTRACT §4 invariant 5). Both
// paths drive the real venv; neither is mocked.
#[test]
fn subprocess_worker_outcomes_and_teardown_match_fork() {
    let Some(python) = python(PythonNeeds::FxVenv).map(std::path::PathBuf::from) else {
        skip_live("`.tiderace-fx-venv` not found — run the Phase-3 Lane-0 env gate first");
        return;
    };
    let _live = fx_support::live_guard(); // serialize corpus-launching scenarios (env isolation)

    let fork_results = sorted_outcomes(run_engine_on_corpus(&python, /* fork */ true));
    let subprocess_results = sorted_outcomes(run_engine_on_corpus(&python, /* fork */ false));

    assert!(!fork_results.is_empty(), "fork path collected/ran nothing");
    assert_eq!(
        fork_results, subprocess_results,
        "no-COW SubprocessWorker outcomes must be identical to the fork path (result-identical)"
    );
}

// --------------------------------------------------------------------------
// Local helpers (kept here so the per-binary helper module stays generic).
// --------------------------------------------------------------------------

/// Collect + execute the whole `fx_corpus` through the engine, returning the
/// per-test results. `fork == true` uses the real `ForkWorker` (Wellspring);
/// `fork == false` uses the no-COW `SubprocessWorker` fallback. Both run the live
/// venv — the python/sqlite boundary is never mocked.
fn run_engine_on_corpus(
    python: &std::path::Path,
    fork: bool,
) -> Vec<engine_core::domain::TestResult> {
    let root = fx_corpus_root();
    let items = RegexCollector::new()
        .collect(&root)
        .expect("collect fx_corpus");
    if fork {
        let mut worker = ForkWorker::launch(python.to_str().unwrap(), &shim(), &root)
            .expect("launch fork worker");
        worker.run(&items).expect("fork run")
    } else {
        let mut worker = SubprocessWorker::new(5_000, num_cpus_or_one()).with_target(
            python.to_str().unwrap(),
            &shim(),
            &root,
        );
        worker.run(&items).expect("subprocess run")
    }
}

/// Sort `(node_id, outcome)` pairs for order-independent comparison between workers.
fn sorted_outcomes(results: Vec<engine_core::domain::TestResult>) -> Vec<(String, Outcome)> {
    let mut v: Vec<(String, Outcome)> = results
        .into_iter()
        .map(|r| (r.node_id.to_string(), r.outcome))
        .collect();
    // `Outcome` is `Eq` but not `Ord`, and node ids are unique, so sort by node id.
    v.sort_by(|a, b| a.0.cmp(&b.0));
    v
}

/// Best-effort CPU count for the subprocess pool; falls back to 1.
fn num_cpus_or_one() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
}
