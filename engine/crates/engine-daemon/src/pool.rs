use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::thread;

use engine_core::domain::{TestItem, TestResult};
#[cfg(unix)]
use engine_core::exec::ForkWorker;
#[cfg(not(unix))]
use engine_core::exec::SubprocessWorker;
use engine_core::exec::Worker;
use engine_core::scheduler::{LocalityScheduler, ScheduleInput, ScheduledTest, Scheduler};

/// Run `items` across a **pool of `workers` in parallel** (the fix for sequential execution —
/// design 06 / ADR-E010). The [`LocalityScheduler`] groups tests by module (scope locality) and
/// LPT-balances the groups across workers; each worker runs its batch on its own thread. The
/// per-batch isolation backend is platform-specific (see [`run_batch`]): fork-per-test on Unix, the
/// no-fork SubprocessWorker (snapshot/restore) on Windows. Coverage rides along if the wellsprings
/// inherit `TIDERACE_COVERAGE` (the caller's env), so impact footprints are still captured.
#[allow(clippy::too_many_arguments)]
pub fn run_parallel(
    python: &str,
    shim: &Path,
    root: &Path,
    items: Vec<TestItem>,
    workers: usize,
    deadline_ms: u64,
    optimistic_no_fork: bool,
    trusted: &HashSet<String>,
) -> Result<Vec<TestResult>, String> {
    if items.is_empty() {
        return Ok(Vec::new());
    }
    let workers = workers.max(1).min(items.len());

    // node id -> item (to rebuild each batch's TestItems from the scheduler's NodeId batches).
    let mut by_node: HashMap<String, TestItem> = items
        .iter()
        .map(|i| (i.node_id.to_string(), i.clone()))
        .collect();
    // Cold run ⇒ no timing history; weight each test equally and group by module for locality.
    let scheduled: Vec<ScheduledTest> = items
        .iter()
        .map(|i| ScheduledTest::new(i.node_id.clone(), locality_key(i.node_id.as_str()), 1))
        .collect();
    let batches = LocalityScheduler::default().plan(&ScheduleInput::new(scheduled, workers));

    let mut handles = Vec::new();
    for batch in batches {
        let batch_items: Vec<TestItem> = batch
            .items()
            .iter()
            .filter_map(|n| by_node.remove(n.as_str()))
            .collect();
        if batch_items.is_empty() {
            continue;
        }
        let (py, sh, rt) = (python.to_string(), shim.to_path_buf(), root.to_path_buf());
        // Only this batch's trusted-pure node ids (the shim only sees this batch).
        let batch_trusted: HashSet<String> = batch_items
            .iter()
            .filter(|it| trusted.contains(it.node_id.as_str()))
            .map(|it| it.node_id.to_string())
            .collect();
        handles.push(thread::spawn(move || -> Result<Vec<TestResult>, String> {
            run_batch(
                &py,
                &sh,
                &rt,
                &batch_items,
                deadline_ms,
                optimistic_no_fork,
                batch_trusted,
            )
        }));
    }

    let mut all = Vec::new();
    for handle in handles {
        all.extend(
            handle
                .join()
                .map_err(|_| "worker thread panicked".to_string())??,
        );
    }
    Ok(all)
}

/// Run one scheduler batch on this thread, using the platform's isolation backend. This is the one
/// place the pool is platform-aware; everything above (scheduling, batching, threading, join) is
/// shared.
///
/// * **Unix** — [`ForkWorker`]: one warm wellspring, fork-per-test (COW isolation, ADR-E003). The
///   optimistic-no-fork ladder + trusted-pure fast path apply here.
/// * **Non-Unix (Windows)** — no `fork()`, so [`SubprocessWorker`] (`--no-fork`) runs the batch
///   in-process with snapshot/restore between tests and refuses opaque modules (sound as of the
///   no-fork isolation fix). One process per batch; parallelism still comes from N batches on N
///   threads, exactly as the fork pool. This is what lets `run --all` / the sub-interpreter tier's
///   fork partition actually run on Windows instead of crashing on `os.fork()`.
#[cfg(unix)]
fn run_batch(
    py: &str,
    sh: &Path,
    rt: &Path,
    batch_items: &[TestItem],
    deadline_ms: u64,
    optimistic_no_fork: bool,
    batch_trusted: std::collections::HashSet<String>,
) -> Result<Vec<TestResult>, String> {
    let mut worker = ForkWorker::launch(py, sh, rt)
        .map_err(|e| format!("failed to launch wellspring: {e}"))?
        .with_deadline_ms(deadline_ms)
        .with_optimistic_no_fork(optimistic_no_fork)
        .with_trusted_pure(batch_trusted);
    worker
        .run(batch_items)
        .map_err(|e| format!("execution failed: {e}"))
}

#[cfg(not(unix))]
fn run_batch(
    py: &str,
    sh: &Path,
    rt: &Path,
    batch_items: &[TestItem],
    deadline_ms: u64,
    _optimistic_no_fork: bool,
    _batch_trusted: std::collections::HashSet<String>,
) -> Result<Vec<TestResult>, String> {
    // The no-fork path always snapshots/restores (its only isolation without COW); the fork-only knobs
    // (optimistic ladder, trusted-pure bare no-fork) don't apply. One process per batch, pool_size 1.
    let mut worker = SubprocessWorker::new(deadline_ms, 1).with_target(py, sh, rt);
    worker
        .run(batch_items)
        .map_err(|e| format!("execution failed: {e}"))
}

/// A test's locality key for scheduling — its module (the file part of the node id), so a module's
/// tests co-locate on one worker and reuse its module/session snapshot.
fn locality_key(node_id: &str) -> String {
    node_id.split("::").next().unwrap_or(node_id).to_string()
}

/// A sensible default worker count: the machine's parallelism, falling back to 4.
pub fn default_workers() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
}

#[cfg(test)]
mod tests {
    use super::{default_workers, locality_key, run_parallel};
    use engine_core::domain::{NodeId, ScopePath, TestItem, TestStyle};
    use engine_core::testing::skip_live;
    use std::collections::HashSet;
    use std::path::{Path, PathBuf};

    fn repo_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../..")
            .canonicalize()
            .expect("repo root")
    }
    // The live test gates on the fx venv (resolved by path), exactly like daemon_e2e — it runs where
    // the venv exists (incl. the coverage CI job) and skips cleanly otherwise.
    fn venv_python() -> Option<PathBuf> {
        let p = repo_root().join(".tiderace-fx-venv/bin/python");
        p.exists().then_some(p)
    }
    /// Any interpreter — the fx venv, else a bare `python3`/`python` on `PATH`. Lets the pool's
    /// platform backend be exercised on **Windows CI** (a bare `setup-python` interpreter, no venv),
    /// which is where the no-fork batch backend actually matters. `None` ⇒ skip.
    fn any_python() -> Option<String> {
        if let Some(v) = venv_python() {
            return Some(v.to_string_lossy().into_owned());
        }
        ["python3", "python"].into_iter().find_map(|cand| {
            std::process::Command::new(cand)
                .arg("--version")
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|_| cand.to_string())
        })
    }
    fn shim() -> PathBuf {
        repo_root().join("engine/py-shim/shim.py")
    }
    fn item(node_id: &str) -> TestItem {
        let module = node_id.split("::").next().unwrap_or(node_id);
        TestItem::new(
            NodeId::new(node_id),
            TestStyle::Function,
            ScopePath::module(module),
        )
    }

    #[test]
    fn locality_key_is_the_module_part_of_the_node_id() {
        assert_eq!(locality_key("pkg/test_a.py::test_x"), "pkg/test_a.py");
        assert_eq!(locality_key("bare"), "bare");
    }

    #[test]
    fn default_workers_is_at_least_one() {
        assert!(default_workers() >= 1);
    }

    #[test]
    fn empty_items_short_circuit_without_launching_a_wellspring() {
        let out = run_parallel(
            "python3",
            Path::new("shim.py"),
            Path::new("/tmp"),
            vec![],
            4,
            5000,
            false,
            &HashSet::new(),
        )
        .expect("empty batch is Ok");
        assert!(out.is_empty());
    }

    #[test]
    fn runs_a_two_module_corpus_across_two_workers() {
        let Some(python) = venv_python() else {
            skip_live("`.tiderace-fx-venv` not present");
            return;
        };
        // Two modules so the LocalityScheduler distributes them across the two workers.
        let dir = std::env::temp_dir().join(format!("tiderace_pool_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("test_a.py"),
            "def test_a1():\n    assert 1 == 1\n\ndef test_a2():\n    assert 2 == 2\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("test_b.py"),
            "def test_b1():\n    assert 3 == 3\n\ndef test_b2():\n    assert 1 == 2\n",
        )
        .unwrap();

        let items = vec![
            item("test_a.py::test_a1"),
            item("test_a.py::test_a2"),
            item("test_b.py::test_b1"),
            item("test_b.py::test_b2"),
        ];
        let results = run_parallel(
            &python.to_string_lossy(),
            &shim(),
            &dir,
            items,
            2,
            5000,
            false,
            &HashSet::new(),
        )
        .expect("pool run succeeds");

        assert_eq!(
            results.len(),
            4,
            "every scheduled test returns exactly one result"
        );
        let mut failed: Vec<String> = results
            .iter()
            .filter(|r| r.outcome.is_failure())
            .map(|r| r.node_id.as_str().to_string())
            .collect();
        failed.sort();
        assert_eq!(
            failed,
            vec!["test_b.py::test_b2".to_string()],
            "only test_b2 fails"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The pool must **isolate module state between tests on whichever backend the platform uses** —
    /// fork on Unix, no-fork SubprocessWorker on Windows. A module-level list mutated by the first
    /// test must not be seen by the second. This is the property that broke silently on the no-fork
    /// path, so run it against a bare interpreter (stdlib corpus, no venv) so **Windows CI** exercises
    /// its own backend here, not just Unix's.
    #[test]
    fn pool_isolates_module_state_between_tests_on_this_platform() {
        let Some(python) = any_python() else {
            skip_live("no Python interpreter available");
            return;
        };
        let dir = std::env::temp_dir().join(format!("tiderace_pool_iso_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // A restorable module that mutates a global: test_b fails iff test_a's append leaked.
        std::fs::write(
            dir.join("test_mut.py"),
            "_SEEN = []\n\
             \n\
             def test_a():\n    _SEEN.append(1)\n    assert _SEEN == [1]\n\
             \n\
             def test_b():\n    _SEEN.append(2)\n    assert _SEEN == [2], f\"LEAK: {_SEEN}\"\n",
        )
        .unwrap();

        let items = vec![item("test_mut.py::test_a"), item("test_mut.py::test_b")];
        let results = run_parallel(
            &python,
            &shim(),
            &dir,
            items,
            1,
            5000,
            false,
            &HashSet::new(),
        )
        .expect("pool run succeeds");

        assert_eq!(results.len(), 2);
        let failures: Vec<&str> = results
            .iter()
            .filter(|r| r.outcome.is_failure())
            .map(|r| r.node_id.as_str())
            .collect();
        assert!(
            failures.is_empty(),
            "pool must restore module state between tests on this platform's backend; leaked in {failures:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
