use std::collections::HashSet;
use std::path::Path;

use engine_core::domain::{TestItem, TestResult};
use engine_core::runner::{run_parallel as core_run_parallel, RunPlan, WorkerStrategy};

/// Run `items` across a **pool of `workers` in parallel** (design 06 / ADR-E010).
///
/// The implementation moved to [`engine_core::runner`] (TID-17) so the CLI could reach the tiers and
/// schedulers this had hardwired; two copies of the scheduling + threading logic would drift. This
/// keeps the daemon's long-standing signature and its platform-default behaviour: locality packing,
/// fork-per-test on Unix, the no-fork `SubprocessWorker` on Windows.
///
/// Coverage rides along if the wellsprings inherit `TIDERACE_COVERAGE` (the caller's env), so impact
/// footprints are still captured.
#[allow(clippy::too_many_arguments)] // long-standing daemon signature, kept for compatibility
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
    let plan = RunPlan {
        strategy: WorkerStrategy::platform_default(),
        workers,
        deadline_ms,
        optimistic_no_fork,
        trusted_pure: trusted.clone(),
        ..RunPlan::default()
    };
    core_run_parallel(python, shim, root, items, &plan)
}

/// A sensible default worker count: the machine's parallelism, falling back to 4.
pub use engine_core::runner::default_workers;

#[cfg(test)]
mod tests {
    use super::{default_workers, run_parallel};
    use engine_core::domain::{NodeId, ScopePath, TestItem, TestStyle};
    use engine_core::runner::locality_key;
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
