//! TID-80 — a module's tests run in one process, in file order, as pytest runs them.
//!
//! Two things used to break that. The collector sorted node ids, so `test_two` ran before
//! `test_one`; and a module heavier than one worker's share was sharded across workers, so a
//! file's tests ran in different processes at once. A file whose tests hand each other state —
//! "upload in one test, list it in the next", the shape of many moto suites — passed under pytest
//! and failed here for no reason its author could see.
//!
//! Sharding is still there behind `shard_modules` for a single-file suite that wants every worker,
//! the way `pytest-xdist --dist load` splits files and `--dist loadfile` keeps them together.
//!
//! The corpus declares a fixture with `@pytest.fixture`, which needs pytest on the interpreter; the
//! Windows CI job runs a bare one, so the live test gates on unix like its siblings.

use engine_core::collection::{Collector, RegexCollector};
use std::path::PathBuf;

/// Every test appends to a list a module-scoped fixture holds; the last one asserts the file ran
/// top to bottom in one process. Named so that alphabetical order is not file order.
///
/// The moto suites this came from keep their state in the mock; `MODULE_GLOBAL` below is the
/// other shape, a counter in the module itself, which TID-81 made carry too.
const CORPUS: &str = "\
import pytest

@pytest.fixture(scope=\"module\")
def order():
    return []

def test_second_in_file(order):
    order.append(1)

def test_first_alphabetically(order):
    order.append(2)

def test_zz_third(order):
    order.append(3)

def test_last_checks(order):
    assert order == [1, 2, 3], f\"not file order in one process: {order}\"
";

/// The same file with its state in a module global: since TID-81 nothing is put back between a
/// file's tests, so this accumulates as it does under pytest. The module after it, on the same
/// worker, must not see what the first one changed in the environment.
const MODULE_GLOBAL: &str = "\
import os

ORDER = []

def test_second_in_file():
    ORDER.append(1)
    os.environ[\"T81_LEAK\"] = \"set in the first module\"

def test_first_alphabetically():
    ORDER.append(2)

def test_last_checks():
    assert ORDER == [1, 2], f\"a file's own globals must carry between its tests: {ORDER}\"
";

const NEXT_MODULE: &str = "\
import os

def test_starts_clean():
    assert \"T81_LEAK\" not in os.environ, \"the previous module's change reached this one\"
";

fn write_corpus(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("tiderace_t80_order_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("test_order.py"), CORPUS).unwrap();
    dir
}

#[test]
fn the_collector_keeps_definition_order_within_a_file() {
    let dir = write_corpus("collect");
    let items = RegexCollector::new().collect(&dir).expect("collection");
    let _ = std::fs::remove_dir_all(&dir);
    let names: Vec<&str> = items
        .iter()
        .map(|i| i.node_id.as_str().rsplit("::").next().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "test_second_in_file",
            "test_first_alphabetically",
            "test_zz_third",
            "test_last_checks"
        ],
        "TID-80: definition order, not alphabetical"
    );
}

/// The live half needs pytest on the interpreter (the corpus declares a fixture with it); the
/// Windows CI job runs a bare one, so it gates on unix like its siblings.
#[cfg(unix)]
mod live {
    use super::write_corpus;
    use engine_core::collection::{Collector, RegexCollector};
    use engine_core::domain::Outcome;
    use engine_core::runner::{run_parallel, RunPlan, SchedulerKind, WorkerStrategy};
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

    #[test]
    fn a_modules_tests_run_in_file_order_in_one_process_with_more_workers_than_tests() {
        let Some(python) = any_python() else {
            skip_live("no Python interpreter available");
            return;
        };
        let dir = write_corpus("run");
        let items = RegexCollector::new().collect(&dir).expect("collection");
        let plan = RunPlan {
            workers: 4,
            strategy: WorkerStrategy::Subprocess,
            scheduler: SchedulerKind::Locality,
            shared_import: false,
            ..RunPlan::default()
        };
        let results = run_parallel(&python, &shim(), &dir, items, &plan).expect("the corpus runs");
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(results.len(), 4);
        for r in &results {
            assert_eq!(
                r.outcome,
                Outcome::Passed,
                "TID-80: {} — the file must run top to bottom in one process: {}",
                r.node_id,
                r.detail
            );
        }
        let workers: std::collections::HashSet<_> =
            results.iter().filter_map(|r| r.worker).collect();
        assert_eq!(workers.len(), 1, "one module, one worker: {workers:?}");
    }

    /// TID-81: a module-level list accumulates across the file's tests, and the module that runs
    /// next on the same worker starts from what the worker found before the first.
    #[test]
    fn a_files_own_globals_carry_between_its_tests_and_not_into_the_next_module() {
        let Some(python) = any_python() else {
            skip_live("no Python interpreter available");
            return;
        };
        let dir = std::env::temp_dir().join(format!("tiderace_t81_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("test_a_counter.py"), super::MODULE_GLOBAL).unwrap();
        std::fs::write(dir.join("test_b_next.py"), super::NEXT_MODULE).unwrap();
        let items = RegexCollector::new().collect(&dir).expect("collection");
        assert_eq!(items.len(), 4);
        let plan = RunPlan {
            workers: 1,
            strategy: WorkerStrategy::Subprocess,
            scheduler: SchedulerKind::Locality,
            shared_import: false,
            ..RunPlan::default()
        };
        let results = run_parallel(&python, &shim(), &dir, items, &plan).expect("the corpus runs");
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(results.len(), 4);
        for r in &results {
            assert_eq!(
                r.outcome,
                Outcome::Passed,
                "TID-81: {} — {}",
                r.node_id,
                r.detail
            );
        }
    }
}
