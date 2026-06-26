use std::collections::HashMap;
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
        handles.push(thread::spawn(move || -> Result<Vec<TestResult>, String> {
            let mut worker = ForkWorker::launch(&py, &sh, &rt)
                .map_err(|e| format!("failed to launch wellspring: {e}"))?
                .with_deadline_ms(deadline_ms)
                .with_optimistic_no_fork(optimistic_no_fork);
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
