//! TID-106 — the worker pool is sized by memory, and each worker's peak memory is reported.
//!
//! A memory limit of one megabyte can budget one worker at most, whatever the CPU count, so a
//! four-worker plan under it runs everything on worker 0 — and each result carries that worker's
//! peak resident size, read from `/proc`, which is how a suite that starts a JVM per worker is
//! seen for what it is. Linux only: that is where the kernel reports both numbers.

#![cfg(target_os = "linux")]

use engine_core::collection::{Collector, RegexCollector};
use engine_core::domain::Outcome;
use engine_core::runner::{run_parallel, RunPlan, WorkerStrategy};
use engine_core::testing::skip_live;
use std::path::PathBuf;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repo root")
}

fn shim() -> PathBuf {
    repo_root().join("engine/py-shim/shim.py")
}

fn any_python() -> Option<String> {
    let venv = repo_root().join(".tiderace-fx-venv/bin/python");
    if venv.exists() {
        return Some(venv.to_string_lossy().into_owned());
    }
    ["python3", "python"]
        .into_iter()
        .find(|cand| {
            std::process::Command::new(cand)
                .arg("--version")
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        })
        .map(str::to_string)
}

fn corpus(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("tiderace_t106_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for m in ["alpha", "beta", "gamma", "delta"] {
        std::fs::write(
            dir.join(format!("test_{m}.py")),
            format!(
                "def test_{m}_one():\n    assert True\n\n\ndef test_{m}_two():\n    assert True\n"
            ),
        )
        .unwrap();
    }
    dir
}

#[test]
fn a_memory_limit_caps_the_pool_and_every_result_carries_its_workers_peak() {
    let Some(python) = any_python() else {
        skip_live("no Python interpreter available");
        return;
    };
    let dir = corpus("limit");
    let items = RegexCollector::new().collect(&dir).expect("collection");
    assert_eq!(items.len(), 8);
    let plan = RunPlan {
        strategy: WorkerStrategy::Fork,
        workers: 4,
        workers_explicit: true, // an explicit count — the limit still caps it
        memory_limit_mb: Some(1),
        ..RunPlan::default()
    };
    let results = run_parallel(&python, &shim(), &dir, items, &plan).expect("the run");
    assert_eq!(results.len(), 8);
    for r in &results {
        assert_eq!(r.outcome, Outcome::Passed, "{}: {}", r.node_id, r.detail);
        assert_eq!(
            r.worker,
            Some(0),
            "TID-106: one megabyte budgets one worker, whatever the count asked for — {}",
            r.node_id
        );
        let peak = r
            .worker_peak_rss_mb
            .expect("the worker's peak resident size is reported");
        assert!(
            (5..20_000).contains(&peak),
            "a Python worker's resident size is a few tens of MB, not {peak}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn without_a_limit_the_default_count_is_kept_when_memory_allows() {
    let Some(python) = any_python() else {
        skip_live("no Python interpreter available");
        return;
    };
    let dir = corpus("free");
    let items = RegexCollector::new().collect(&dir).expect("collection");
    // Four modules, four workers: a tiny image on any machine that runs this suite budgets far
    // more than four, so every module gets its own worker and the schedule shows all four lanes.
    let plan = RunPlan {
        strategy: WorkerStrategy::Fork,
        workers: 4,
        ..RunPlan::default()
    };
    let results = run_parallel(&python, &shim(), &dir, items, &plan).expect("the run");
    let lanes: std::collections::BTreeSet<usize> =
        results.iter().filter_map(|r| r.worker).collect();
    assert_eq!(lanes.len(), 4, "{lanes:?}");
    assert!(results.iter().all(|r| r.worker_peak_rss_mb.is_some()));
    let _ = std::fs::remove_dir_all(&dir);
}
