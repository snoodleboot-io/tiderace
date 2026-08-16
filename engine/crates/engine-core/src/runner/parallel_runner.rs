use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::thread;

use crate::domain::{TestItem, TestResult};
use crate::exec::{probe_modules, ForkWorker, SubInterpWorker, SubprocessWorker, Worker};
use crate::runner::{RunPlan, WorkerStrategy};
use crate::scheduler::{ScheduleInput, ScheduledTest};

/// Run `items` across a pool of workers in parallel, using the tier and scheduler named by `plan`
/// (TID-17).
///
/// Previously this lived in `engine-daemon` hardwired to the locality scheduler and the platform's
/// default tier, which is why the CLI could not reach the other tiers at all. It is parameterised
/// and lives in `engine-core` so the daemon and the CLI share one implementation rather than
/// drifting apart.
///
/// The scheduler partitions the corpus and each batch runs on its own thread. The one exception is
/// the sub-interpreter tier, which is itself a pool — see [`run_subinterp_hybrid`].
pub fn run_parallel(
    python: &str,
    shim: &Path,
    root: &Path,
    items: Vec<TestItem>,
    plan: &RunPlan,
) -> Result<Vec<TestResult>, String> {
    if items.is_empty() {
        return Ok(Vec::new());
    }
    if !plan.strategy.is_available() {
        return Err(format!(
            "the {} tier is not available on this platform",
            plan.strategy
        ));
    }
    if plan.strategy.is_hybrid() {
        return run_subinterp_hybrid(python, shim, root, items, plan);
    }
    run_batched(python, shim, root, items, plan, plan.strategy)
}

/// Schedule `items` into batches and run each on its own thread with `strategy`.
fn run_batched(
    python: &str,
    shim: &Path,
    root: &Path,
    items: Vec<TestItem>,
    plan: &RunPlan,
    strategy: WorkerStrategy,
) -> Result<Vec<TestResult>, String> {
    if items.is_empty() {
        return Ok(Vec::new());
    }
    let workers = plan.effective_workers(items.len());

    // node id -> item, to rebuild each batch's TestItems from the scheduler's NodeId batches.
    let mut by_node: HashMap<String, TestItem> = items
        .iter()
        .map(|i| (i.node_id.to_string(), i.clone()))
        .collect();
    // Cold run ⇒ no timing history; weight each test equally and group by module for locality.
    let scheduled: Vec<ScheduledTest> = items
        .iter()
        .map(|i| ScheduledTest::new(i.node_id.clone(), locality_key(i.node_id.as_str()), 1))
        .collect();
    let batches = plan
        .scheduler
        .build()
        .plan(&ScheduleInput::new(scheduled, workers));

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
            .filter(|it| plan.trusted_pure.contains(it.node_id.as_str()))
            .map(|it| it.node_id.to_string())
            .collect();
        let exec = BatchExec {
            strategy,
            deadline_ms: plan.deadline_ms,
            optimistic_no_fork: plan.optimistic_no_fork,
        };
        handles.push(thread::spawn(move || -> Result<Vec<TestResult>, String> {
            run_batch(exec, &py, &sh, &rt, &batch_items, batch_trusted)
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

/// The sub-interpreter tier (ADR-E015 / TID-11): probe each module, run the **safe** subset on one
/// parallel sub-interpreter pool, and send everything else to the platform fallback.
///
/// It is hybrid by necessity rather than by policy. A sub-interpreter cannot load a single-phase C
/// extension — numpy's `_multiarray_umath` is the canonical refusal — so "run this whole corpus on
/// sub-interpreters" is not a configuration that exists for any corpus with a compiled dependency.
///
/// The pool is **not** wrapped in the batching above: `SubInterpWorker` takes a whole batch and fans
/// it out across its own interpreters in one process, so threading it per scheduler batch would
/// nest two pools and oversubscribe the machine.
///
/// A probe that cannot classify a module (CPython < 3.14, no probe API) returns `None`, and `None`
/// routes to the fallback — always sound, never wrong, just not accelerated.
fn run_subinterp_hybrid(
    python: &str,
    shim: &Path,
    root: &Path,
    items: Vec<TestItem>,
    plan: &RunPlan,
) -> Result<Vec<TestResult>, String> {
    let mut modules: Vec<String> = items
        .iter()
        .map(|i| locality_key(i.node_id.as_str()))
        .collect();
    modules.sort();
    modules.dedup();

    let verdicts = probe_modules(python, shim, root, &modules)?;
    let (safe_items, rest): (Vec<TestItem>, Vec<TestItem>) = items.into_iter().partition(|it| {
        verdicts
            .get(&locality_key(it.node_id.as_str()))
            .copied()
            .flatten()
            .unwrap_or(false)
    });

    let mut all = Vec::new();
    if !safe_items.is_empty() {
        let mut worker = SubInterpWorker::new(plan.deadline_ms)
            .with_target(python, shim, root)
            .with_pool_size(plan.effective_workers(safe_items.len()));
        all.extend(
            worker
                .run(&safe_items)
                .map_err(|e| format!("subinterp pool: {e}"))?,
        );
    }
    if !rest.is_empty() {
        all.extend(run_batched(
            python,
            shim,
            root,
            rest,
            plan,
            plan.strategy.fallback(),
        )?);
    }
    Ok(all)
}

/// The per-batch execution settings, split out so a batch can be handed across a thread boundary as
/// one `Copy` value instead of a fistful of positional scalars.
#[derive(Debug, Clone, Copy)]
struct BatchExec {
    strategy: WorkerStrategy,
    deadline_ms: u64,
    optimistic_no_fork: bool,
}

/// Run one scheduler batch on this thread with the named tier.
fn run_batch(
    exec: BatchExec,
    py: &str,
    sh: &Path,
    rt: &Path,
    batch_items: &[TestItem],
    batch_trusted: HashSet<String>,
) -> Result<Vec<TestResult>, String> {
    let BatchExec {
        strategy,
        deadline_ms,
        optimistic_no_fork,
    } = exec;
    match strategy {
        #[cfg(unix)]
        WorkerStrategy::Fork => {
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
        WorkerStrategy::Fork => Err("fork is unavailable on this platform".to_string()),
        // The no-fork path always snapshots/restores (its only isolation without COW); the fork-only
        // knobs (optimistic ladder, trusted-pure bare no-fork) do not apply. One process per batch.
        WorkerStrategy::Subprocess => {
            let mut worker = SubprocessWorker::new(deadline_ms, 1).with_target(py, sh, rt);
            worker
                .run(batch_items)
                .map_err(|e| format!("execution failed: {e}"))
        }
        // Routed before batching; reaching here would mean a nested pool.
        WorkerStrategy::SubInterp => {
            Err("the subinterp tier is routed before batching, not per batch".to_string())
        }
    }
}

/// A test's locality key for scheduling — its module (the file part of the node id), so a module's
/// tests co-locate on one worker and reuse its module/session snapshot.
pub fn locality_key(node_id: &str) -> String {
    node_id.split("::").next().unwrap_or(node_id).to_string()
}

#[cfg(test)]
mod tests {
    use super::{locality_key, run_parallel};
    use crate::runner::RunPlan;
    #[cfg(not(unix))]
    use crate::runner::WorkerStrategy;
    use std::path::Path;

    #[test]
    fn locality_key_is_the_module_path() {
        assert_eq!(locality_key("pkg/test_x.py::C::t"), "pkg/test_x.py");
        assert_eq!(locality_key("test_x.py::t"), "test_x.py");
        // A node id with no separator is its own key rather than an empty string, which would
        // collapse every such test into one locality group.
        assert_eq!(locality_key("test_x.py"), "test_x.py");
    }

    #[test]
    fn an_empty_corpus_is_not_an_error() {
        let plan = RunPlan::default();
        let out = run_parallel(
            "python3",
            Path::new("shim.py"),
            Path::new("."),
            Vec::new(),
            &plan,
        );
        assert_eq!(out.expect("empty corpus runs"), Vec::new());
    }

    /// An unavailable tier must be refused up front with a message naming it, not fail per batch
    /// deep inside a worker thread where it reads as an execution error.
    #[cfg(not(unix))]
    #[test]
    fn requesting_fork_without_fork_is_refused_clearly() {
        use crate::domain::{NodeId, TestItem, TestStyle};
        let plan = RunPlan {
            strategy: WorkerStrategy::Fork,
            ..RunPlan::default()
        };
        let items = vec![TestItem::new(NodeId::new("t.py::a"), TestStyle::Function)];
        let err = run_parallel(
            "python3",
            Path::new("shim.py"),
            Path::new("."),
            items,
            &plan,
        )
        .expect_err("fork must be refused where it does not exist");
        assert!(
            err.contains("fork"),
            "message must name the tier; got {err:?}"
        );
    }
}
