//! TID-52 — work is handed out from a queue, not partitioned up front.
//!
//! The scheduler used to commit each worker's whole share before anything ran, weighting every test
//! equally because a cold run has no timing history. On pirn-agents, whose per-test cost spans four
//! orders of magnitude, bins balanced by test count ran 121/97/66/34/31/25/23/19 seconds: 57% of the
//! machine idle, 2.32x the makespan a perfectly balanced run would take, and the reason pytest-xdist
//! — which distributes dynamically — beat us there while losing everywhere else.
//!
//! The assertion here is structural rather than a stopwatch, because a stopwatch on a shared machine
//! measures the machine. A static partition of M modules over W workers can never give one worker
//! more than `ceil(M / W)` modules. A queue can, and must: that is precisely what "a worker that
//! finished early took the next one" looks like from outside.
//!
//! The subprocess tier is used because its worker is one process for its whole lifetime, so the pid
//! a test reports *is* the worker's identity. On the fork tier a demoted test runs in a fork of its
//! worker and would report a pid of its own.

#![cfg(unix)]

use engine_core::collection::{Collector, RegexCollector};
use engine_core::domain::Outcome;
use engine_core::runner::{run_parallel, RunPlan, SchedulerKind, WorkerStrategy};
use engine_core::testing::skip_live;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

const MODULES: usize = 12;
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

/// Twelve modules, one of which is slow. Every test records `module pid` to a file on disk — a
/// module-level list would be rolled back by the in-process restore, and the runner's own results
/// carry no worker identity.
fn write_project() -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "tiderace_t52_{}_{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let tests = dir.join("tests");
    std::fs::create_dir_all(&tests).unwrap();
    let log = tests.join("who.log");
    std::fs::write(
        tests.join("conftest.py"),
        format!(
            "import os\n\nLOG = {log:?}\n\n\
             def note(module):\n\
             \x20   with open(LOG, 'a') as fh:\n\
             \x20       fh.write(f'{{module}} {{os.getpid()}}\\n')\n",
            log = log.to_string_lossy()
        ),
    )
    .unwrap();
    for i in 0..MODULES {
        // One slow module. Under a static partition its worker also owns a sixth of everything else
        // and finishes last; under a queue the other worker drains what is left.
        let body = if i == 0 {
            "import time\n\ndef test_slow():\n    note('m0')\n    time.sleep(1.5)\n"
        } else {
            "def test_quick():\n    note('m{i}')\n"
        };
        std::fs::write(
            tests.join(format!("test_m{i}.py")),
            format!(
                "from conftest import note\n\n{}",
                body.replace("{i}", &i.to_string())
            ),
        )
        .unwrap();
    }
    dir
}

/// `module -> pid`, from the log every test appended to.
fn who_ran_what(log: &PathBuf) -> HashMap<String, String> {
    std::fs::read_to_string(log)
        .expect("every test recorded which process ran it")
        .lines()
        .filter_map(|l| l.split_once(' '))
        .map(|(m, pid)| (m.to_string(), pid.to_string()))
        .collect()
}

#[test]
fn a_worker_that_finishes_early_takes_more_modules_than_its_static_share() {
    let Some(python) = any_python() else {
        skip_live("no Python interpreter available");
        return;
    };
    let dir = write_project();
    let tests = dir.join("tests");
    let items = RegexCollector::new().collect(&tests).expect("collection");
    assert_eq!(items.len(), MODULES, "one test per module: {items:?}");

    let plan = RunPlan {
        workers: WORKERS,
        strategy: WorkerStrategy::Subprocess,
        scheduler: SchedulerKind::Locality,
        shared_import: false,
        ..RunPlan::default()
    };
    let results = run_parallel(&python, &shim(), &tests, items, &plan).expect("the corpus runs");
    assert_eq!(results.len(), MODULES, "every test reports exactly once");
    for r in &results {
        assert_eq!(r.outcome, Outcome::Passed, "{}: {}", r.node_id, r.detail);
    }

    let by_module = who_ran_what(&tests.join("who.log"));
    assert_eq!(
        by_module.len(),
        MODULES,
        "each module logged once: {by_module:?}"
    );
    let mut per_pid: HashMap<&str, usize> = HashMap::new();
    for pid in by_module.values() {
        *per_pid.entry(pid.as_str()).or_default() += 1;
    }
    let busiest = *per_pid.values().max().expect("some worker ran something");
    let static_share = MODULES.div_ceil(WORKERS);
    assert!(
        busiest > static_share,
        "TID-52: a static partition of {MODULES} modules over {WORKERS} workers gives no worker \
         more than {static_share}; under a queue the worker that was not stuck behind the slow \
         module takes the rest. Busiest worker ran {busiest} — modules per pid: {per_pid:?}"
    );
    assert!(
        per_pid.len() > 1,
        "both workers did work; one process for everything would not be parallel at all: {per_pid:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The queue must not change *what* runs. Many more modules than workers, every result once.
#[test]
fn every_test_still_runs_exactly_once_when_units_outnumber_workers() {
    let Some(python) = any_python() else {
        skip_live("no Python interpreter available");
        return;
    };
    let dir = write_project();
    let tests = dir.join("tests");
    let items = RegexCollector::new().collect(&tests).expect("collection");
    let plan = RunPlan {
        workers: 4,
        strategy: WorkerStrategy::Subprocess,
        shared_import: false,
        ..RunPlan::default()
    };
    let results = run_parallel(&python, &shim(), &tests, items, &plan).expect("the corpus runs");
    let ids: HashSet<&str> = results.iter().map(|r| r.node_id.as_str()).collect();
    assert_eq!(
        ids.len(),
        MODULES,
        "{MODULES} distinct node ids, none run twice by two workers racing the queue: {:?}",
        results
            .iter()
            .map(|r| r.node_id.as_str())
            .collect::<Vec<_>>()
    );
    let _ = std::fs::remove_dir_all(&dir);
}
