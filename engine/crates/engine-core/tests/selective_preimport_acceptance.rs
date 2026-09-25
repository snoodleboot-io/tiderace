//! TID-75 — a run imports only the modules it will execute.
//!
//! The shim's start-up imported every test module in the suite before a single worker existed —
//! ~4s on pirn-agents, and the whole of a one-test run after an edit. The runner now hands every
//! worker a file naming the modules its items live in, and `_preimport` / `_discover` import those
//! and the conftests above them. A full run names every module and is unchanged.
//!
//! Observable from outside because importing is a side effect: `test_b.py` writes a marker file
//! when it is imported. Run only `test_a.py`'s test and the marker must not appear; run both and it
//! must. On every tier, since each tier launches its own worker.

use engine_core::collection::{Collector, RegexCollector};
use engine_core::domain::{Outcome, TestItem};
use engine_core::runner::{run_parallel, RunPlan, WorkerStrategy};
use engine_core::testing::skip_live;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

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

fn scratch(tag: &str) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "tiderace_t75_{tag}_{}_{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_project(dir: &Path) -> PathBuf {
    let marker = dir.join("b_was_imported");
    std::fs::write(dir.join("test_a.py"), "def test_a():\n    assert True\n").unwrap();
    std::fs::write(
        dir.join("test_b.py"),
        format!(
            "open({:?}, 'w').close()  # importing this module is the observable side effect\n\n\
             def test_b():\n    assert True\n",
            marker.to_string_lossy()
        ),
    )
    .unwrap();
    marker
}

fn run(python: &str, dir: &Path, items: Vec<TestItem>, plan: &RunPlan) {
    let results = run_parallel(python, &shim(), dir, items, plan).expect("the run completes");
    for r in &results {
        assert_eq!(r.outcome, Outcome::Passed, "{}: {}", r.node_id, r.detail);
    }
}

fn plans() -> Vec<(&'static str, RunPlan)> {
    let mut plans = vec![(
        "subprocess",
        RunPlan {
            strategy: WorkerStrategy::Subprocess,
            shared_import: false,
            workers: 1,
            ..RunPlan::default()
        },
    )];
    if cfg!(unix) {
        plans.push((
            "fork, shared-import pool",
            RunPlan {
                strategy: WorkerStrategy::Fork,
                shared_import: true,
                workers: 2,
                ..RunPlan::default()
            },
        ));
        plans.push((
            "fork, own wellspring",
            RunPlan {
                strategy: WorkerStrategy::Fork,
                shared_import: false,
                workers: 1,
                ..RunPlan::default()
            },
        ));
    }
    plans
}

#[test]
fn a_run_imports_only_the_modules_it_executes_on_every_tier() {
    let Some(python) = any_python() else {
        skip_live("no Python interpreter available");
        return;
    };
    for (label, plan) in plans() {
        let dir = scratch("selective");
        let marker = write_project(&dir);
        let items = RegexCollector::new().collect(&dir).expect("collection");
        assert_eq!(items.len(), 2);

        let only_a: Vec<TestItem> = items
            .iter()
            .filter(|i| i.node_id.as_str().starts_with("test_a.py"))
            .cloned()
            .collect();
        run(&python, &dir, only_a, &plan);
        assert!(
            !marker.exists(),
            "TID-75 [{label}]: only test_a's test ran, so test_b.py must not have been imported — \
             the start-up imported the whole suite for a one-module run"
        );

        run(&python, &dir, items, &plan);
        assert!(
            marker.exists(),
            "[{label}]: a run that executes test_b imports test_b.py — the full run is unchanged"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
