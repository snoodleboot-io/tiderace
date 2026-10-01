//! TID-84 — a persistent pool parent imports the suite once and forks fresh workers per run.
//!
//! Every `tiderace run` paid the suite's import: the pool parent imported it and exited with the
//! run. A parent launched with `launch_persistent` stays, and `run_parallel_with_pool` takes its
//! workers from it — so the second run imports nothing, and each run's workers start from the same
//! clean image rather than from the previous run's state.

#![cfg(unix)]

use engine_core::collection::{Collector, RegexCollector};
use engine_core::domain::Outcome;
use engine_core::exec::WellspringPool;
use engine_core::runner::{
    run_parallel_with_pool, ForkOptions, Learned, RunPlan, SchedulerKind, WorkerCount,
    WorkerStrategy,
};
use engine_core::testing::{python, shim, skip_live, PythonNeeds};
use std::path::{Path, PathBuf};

/// The test module appends a line to `imports.log` whenever it is imported, and its first test
/// bumps a module global from the value the image holds — which is only what it sees if every run
/// starts from the image rather than from a worker the previous run left behind. The second test
/// sees the bump: a file's tests share one process, in file order (TID-80/81), as under pytest.
fn write_corpus() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("tiderace_t84_pool_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let tests = dir.join("tests");
    std::fs::create_dir_all(&tests).unwrap();
    std::fs::write(
        tests.join("test_warm.py"),
        "import os\n\
         with open(os.path.join(os.path.dirname(__file__), 'imports.log'), 'a') as f:\n    \
             f.write('imported\\n')\n\
         COUNTER = [0]\n\n\
         def test_a_bumps_from_the_image():\n    COUNTER[0] += 1\n    assert COUNTER[0] == 1\n\n\
         def test_b_runs_after_a_in_the_same_process():\n    assert COUNTER[0] == 1\n",
    )
    .unwrap();
    dir
}

fn imports(dir: &Path) -> usize {
    std::fs::read_to_string(dir.join("tests/imports.log"))
        .map(|s| s.lines().count())
        .unwrap_or(0)
}

#[test]
fn a_persistent_parent_imports_once_and_every_run_forks_from_the_clean_image() {
    let Some(python) = python(PythonNeeds::Any) else {
        skip_live("no Python interpreter available");
        return;
    };
    let dir = write_corpus();
    let tests = dir.join("tests");
    let items = RegexCollector::new().collect(&tests).expect("collection");
    assert_eq!(items.len(), 2);

    let mut pool =
        WellspringPool::launch_persistent(&python, &shim(), &tests, true).expect("warm image");
    assert!(pool.is_persistent());
    assert_eq!(imports(&dir), 1, "the launch imported the suite once");

    let plan = RunPlan {
        fork: ForkOptions {
            shared_import: true,
            ..ForkOptions::default()
        },
        workers: WorkerCount::Default(1),
        strategy: WorkerStrategy::Fork,
        scheduler: SchedulerKind::Locality,
        ..RunPlan::default()
    };
    for round in 1..=3 {
        let results = run_parallel_with_pool(
            &python,
            &shim(),
            &tests,
            items.clone(),
            &plan,
            &Learned::default(),
            &mut pool,
        )
        .unwrap_or_else(|e| panic!("run {round}: {e}"));
        assert_eq!(results.len(), 2, "run {round}");
        for r in &results {
            assert_eq!(
                r.outcome,
                Outcome::Passed,
                "run {round}: {} — {}",
                r.node_id,
                r.detail
            );
        }
        assert_eq!(
            imports(&dir),
            1,
            "run {round} imported nothing: the image is warm"
        );
        assert!(pool.is_alive(), "the parent outlives run {round}");
    }
    drop(pool);
    let _ = std::fs::remove_dir_all(&dir);
}
