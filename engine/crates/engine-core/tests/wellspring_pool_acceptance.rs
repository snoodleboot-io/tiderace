//! TID-4 — one imported image, N forked workers.
//!
//! The pool of N independent `python <shim>` processes imported the project N times. This asserts
//! the pool produces the same results while doing that work once.

#![cfg(unix)]

use engine_core::collection::{Collector, RegexCollector};
use engine_core::domain::Outcome;
use engine_core::exec::{PooledWorker, WellspringPool, Worker};
use engine_core::testing::skip_live;
use std::path::PathBuf;
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

/// A module whose import is *observable*: it stamps the importing process's pid into a global. If
/// the pool works, every worker inherits the same stamp, because the import happened once in their
/// shared parent. If each worker imported for itself, the stamps would differ.
fn write_corpus(tag: &str, modules: usize) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "tiderace_t4_{tag}_{}_{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("importer.py"),
        "import os\n\nIMPORTED_BY = os.getpid()\n",
    )
    .unwrap();
    for m in 0..modules {
        let body = format!(
            "from importer import IMPORTED_BY\n\
             \n\
             \n\
             def test_a_{m}():\n\
             \x20   assert {m} + 1 == {}\n\
             \n\
             \n\
             def test_importer_ran_once_{m}():\n\
             \x20   # Written into the result detail so the test itself does not depend on the answer.\n\
             \x20   assert IMPORTED_BY > 0\n",
            m + 1
        );
        std::fs::write(dir.join(format!("test_m{m}.py")), body).unwrap();
    }
    dir
}

/// The pool runs a corpus to the same answers as any other tier.
#[test]
fn a_pooled_run_produces_the_same_results() {
    let Some(python) = any_python() else {
        skip_live("no Python interpreter available");
        return;
    };
    let dir = write_corpus("results", 6);
    let items = RegexCollector::new().collect(&dir).expect("collection");
    assert_eq!(items.len(), 12, "2 tests in each of 6 modules");

    let mut pool = WellspringPool::launch(&python, &shim(), &dir, true, 4).expect("pool launches");
    assert_eq!(pool.available(), 4, "all four workers connected");

    // Drive every worker, splitting the corpus the way the scheduler would.
    let mut all = Vec::new();
    let chunks: Vec<Vec<_>> = items.chunks(3).map(|c| c.to_vec()).collect();
    for chunk in chunks {
        let transport = pool.take_worker().expect("a worker is available");
        let mut worker = PooledWorker::new(transport, 20_000);
        all.extend(worker.run(&chunk).expect("pooled batch runs"));
    }
    assert_eq!(all.len(), 12, "one result per test");
    for r in &all {
        assert_eq!(
            r.outcome,
            Outcome::Passed,
            "{} — {}",
            r.node_id.as_str(),
            r.detail
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// The point of the pool: the project is imported **once**, not once per worker.
///
/// Asserted structurally rather than by timing. `importer.py` records the pid that imported it, and
/// every worker is a fork of the process that did — so all workers report the same pid, and it is
/// not their own. N independent wellsprings could not produce that.
#[test]
fn the_project_is_imported_once_for_the_whole_pool() {
    let Some(python) = any_python() else {
        skip_live("no Python interpreter available");
        return;
    };
    let dir = write_corpus("importonce", 1);
    std::fs::write(
        dir.join("test_report.py"),
        "import os\n\nfrom importer import IMPORTED_BY\n\
         \n\
         \n\
         def test_reports_importer_pid():\n\
         \x20   # Fails on purpose: the detail is how the pid escapes to the Rust side.\n\
         \x20   assert False, f\"importer={IMPORTED_BY} worker_parent={os.getppid()}\"\n",
    )
    .unwrap();
    let items: Vec<_> = RegexCollector::new()
        .collect(&dir)
        .expect("collection")
        .into_iter()
        .filter(|i| i.node_id.as_str().contains("test_reports_importer_pid"))
        .collect();
    assert_eq!(items.len(), 1);

    let mut pool = WellspringPool::launch(&python, &shim(), &dir, true, 3).expect("pool launches");
    let mut seen = Vec::new();
    for _ in 0..3 {
        let transport = pool.take_worker().expect("worker");
        let mut worker = PooledWorker::new(transport, 20_000);
        let out = worker.run(&items).expect("batch runs");
        let detail = out[0].detail.clone();
        let importer = detail
            .split("importer=")
            .nth(1)
            .and_then(|s| s.split_whitespace().next())
            .expect("the detail carries the importing pid")
            .to_string();
        seen.push(importer);
    }
    assert_eq!(seen.len(), 3);
    assert!(
        seen.iter().all(|p| *p == seen[0]),
        "TID-4: every worker must share one imported image; got {seen:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
