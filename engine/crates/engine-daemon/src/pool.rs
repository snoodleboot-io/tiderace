use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::thread;

use engine_core::domain::{TestItem, TestResult};
use engine_core::exec::{ForkWorker, Worker};
use engine_core::scheduler::{LocalityScheduler, ScheduleInput, ScheduledTest, Scheduler};

/// Run `items` across a **pool of `workers` wellsprings in parallel** (the fix for sequential
/// execution — design 06 / ADR-E010). The [`LocalityScheduler`] groups tests by module (scope
/// locality) and LPT-balances the groups across workers; each worker is its own warm wellspring on its
/// own thread, forking per test (ADR-E003 isolation preserved). Coverage rides along if the wellsprings
/// inherit `RIPTIDE_COVERAGE` (the caller's env), so impact footprints are still captured.
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
            let mut worker = ForkWorker::launch(&py, &sh, &rt)
                .map_err(|e| format!("failed to launch wellspring: {e}"))?
                .with_deadline_ms(deadline_ms)
                .with_optimistic_no_fork(optimistic_no_fork)
                .with_trusted_pure(batch_trusted);
            worker
                .run(&batch_items)
                .map_err(|e| format!("execution failed: {e}"))
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
        let p = repo_root().join(".riptide-fx-venv/bin/python");
        p.exists().then_some(p)
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
            skip_live("`.riptide-fx-venv` not present");
            return;
        };
        // Two modules so the LocalityScheduler distributes them across the two workers.
        let dir = std::env::temp_dir().join(format!("riptide_pool_{}", std::process::id()));
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
}
