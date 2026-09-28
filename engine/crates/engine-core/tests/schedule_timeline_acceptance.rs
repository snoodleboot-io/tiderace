//! TID-78 — every result from the parallel runner says where and when it ran.
//!
//! A run's wall clock is a consequence of its schedule: which unit ran on which worker, from when
//! to when. Without that in the report, a tail cannot be told apart from a slow test, an unlucky
//! ordering, or a wrong weight. The runner stamps each unit's slot on its results as it pops the
//! unit and as the results come back, and `benchmarks/harness/timeline.py` draws them.

use engine_core::collection::{Collector, RegexCollector};
use engine_core::domain::Outcome;
use engine_core::runner::{run_parallel, RunPlan, SchedulerKind, WorkerStrategy};
use engine_core::testing::skip_live;
use std::collections::BTreeMap;
use std::path::PathBuf;

const MODULES: usize = 6;
const WORKERS: usize = 2;

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
    for cand in ["python3", "python"] {
        let ok = std::process::Command::new(cand)
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if ok {
            return Some(cand.to_string());
        }
    }
    None
}

/// Six modules of two tests each, every test sleeping long enough that a unit's span is measurable.
fn write_project() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("tiderace_t78_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let tests = dir.join("tests");
    std::fs::create_dir_all(&tests).unwrap();
    for m in 0..MODULES {
        std::fs::write(
            tests.join(format!("test_m{m}.py")),
            "import time\n\n\ndef test_a():\n    time.sleep(0.05)\n\n\ndef test_b():\n    time.sleep(0.05)\n",
        )
        .unwrap();
    }
    dir
}

#[test]
fn every_result_carries_its_worker_and_unit_slot_and_lanes_do_not_overlap() {
    let Some(python) = any_python() else {
        skip_live("no Python interpreter available");
        return;
    };
    let dir = write_project();
    let tests = dir.join("tests");
    let items = RegexCollector::new().collect(&tests).expect("collection");
    assert_eq!(items.len(), MODULES * 2);

    let plan = RunPlan {
        workers: WORKERS,
        strategy: WorkerStrategy::Subprocess,
        scheduler: SchedulerKind::Locality,
        shared_import: false,
        ..RunPlan::default()
    };
    let results = run_parallel(&python, &shim(), &tests, items, &plan).expect("the corpus runs");
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(results.len(), MODULES * 2);

    // (unit) → (worker, start, end): every node in a unit agrees on its slot.
    let mut units: BTreeMap<usize, (usize, u64, u64)> = BTreeMap::new();
    for r in &results {
        assert_eq!(r.outcome, Outcome::Passed, "{}: {}", r.node_id, r.detail);
        let (Some(w), Some(u), Some(s), Some(e)) =
            (r.worker, r.unit, r.unit_started_ms, r.unit_ended_ms)
        else {
            panic!("TID-78: {} carries no schedule slot: {r:?}", r.node_id);
        };
        assert!(w < WORKERS, "{}: worker {w} of {WORKERS}", r.node_id);
        assert!(
            s <= e,
            "{}: unit {u} ends ({e}ms) before it starts ({s}ms)",
            r.node_id
        );
        assert!(
            e - s >= 100,
            "{}: unit {u} spans {}ms but holds two 50ms tests",
            r.node_id,
            e - s
        );
        let slot = units.entry(u).or_insert((w, s, e));
        assert_eq!(
            *slot,
            (w, s, e),
            "{}: nodes of unit {u} disagree on its slot",
            r.node_id
        );
    }
    assert_eq!(units.len(), MODULES, "one unit per module: {units:?}");

    // A worker runs one unit at a time: within a lane, sorted by start, no unit begins before the
    // previous one ended.
    let mut lanes: BTreeMap<usize, Vec<(u64, u64)>> = BTreeMap::new();
    for &(w, s, e) in units.values() {
        lanes.entry(w).or_default().push((s, e));
    }
    assert_eq!(lanes.len(), WORKERS, "both workers took units: {lanes:?}");
    for (w, lane) in lanes.iter_mut() {
        lane.sort_unstable();
        for pair in lane.windows(2) {
            assert!(
                pair[1].0 >= pair[0].1,
                "worker {w}: unit starting at {}ms overlaps one ending at {}ms",
                pair[1].0,
                pair[0].1
            );
        }
    }
}
