use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::thread;

use crate::domain::{TestItem, TestResult};
#[cfg(unix)]
use crate::exec::{ForkWorker, PooledWorker, WellspringPool};
use crate::exec::{SafeSetCache, SubInterpWorker, SubprocessWorker, Worker};
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

    // TID-4: one imported image, forked per worker. Stood up before the batch loop so the import is
    // finished — and paid once — before any worker starts. Fork-tier only: the subprocess and
    // sub-interpreter tiers have no wellspring to share, by construction.
    #[cfg(unix)]
    let mut pool = if plan.shared_import && matches!(strategy, WorkerStrategy::Fork) {
        // Launched with restore unconditionally, exactly as `ForkWorker::launch_optimistic` does:
        // it costs nothing when the ladder is off, and it makes the unsound combination — in-process
        // execution with no snapshot — unreachable rather than merely unused.
        Some(WellspringPool::launch(python, shim, root, true, workers).map_err(|e| e.to_string())?)
    } else {
        None
    };

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
        // Likewise only this batch's recorded offenders (TID-33).
        let batch_must_fork: HashSet<String> = batch_items
            .iter()
            .filter(|it| plan.must_fork.contains(it.node_id.as_str()))
            .map(|it| it.node_id.to_string())
            .collect();
        let exec = BatchExec {
            strategy,
            deadline_ms: plan.deadline_ms,
            optimistic_no_fork: plan.optimistic_no_fork,
        };
        // A pooled transport is owned outright, so it moves into the thread without borrowing the
        // pool. The pool itself must outlive the threads — it is dropped after the joins below,
        // because its parent process only exits once every worker connection has closed.
        #[cfg(unix)]
        let pooled = pool.as_mut().and_then(|p| p.take_worker());
        #[cfg(not(unix))]
        let pooled: Option<()> = None;

        handles.push(thread::spawn(move || -> Result<Vec<TestResult>, String> {
            #[cfg(unix)]
            if let Some(transport) = pooled {
                let mut worker = PooledWorker::new(transport, exec.deadline_ms)
                    .with_optimistic_no_fork(exec.optimistic_no_fork)
                    .with_trusted_pure(batch_trusted)
                    .with_must_fork(batch_must_fork);
                return worker
                    .run(&batch_items)
                    .map_err(|e| format!("execution failed: {e}"));
            }
            #[cfg(not(unix))]
            let _ = pooled;
            run_batch(
                exec,
                &py,
                &sh,
                &rt,
                &batch_items,
                batch_trusted,
                batch_must_fork,
            )
        }));
    }

    let mut all = Vec::new();
    let mut first_err = None;
    for handle in handles {
        match handle.join() {
            Ok(Ok(results)) => all.extend(results),
            Ok(Err(e)) => {
                first_err.get_or_insert(e);
            }
            Err(_) => {
                first_err.get_or_insert_with(|| "worker thread panicked".to_string());
            }
        }
    }
    // Every worker connection is closed by now (the threads owned them), so the pool's parent can
    // exit. Dropping it here rather than on the `?` path above is what keeps a failing run from
    // leaving an orphaned parent behind holding the imported image.
    #[cfg(unix)]
    drop(pool.take());
    match first_err {
        Some(e) => Err(e),
        None => Ok(all),
    }
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

    // Probing means launching a fresh interpreter per module, so it is cached by content hash and
    // only new or changed modules pay (TID-35). Without this the CLI re-probed the whole corpus on
    // every invocation, which on a small module count is most of this tier's cost — and it hurt
    // most on Windows, the one platform the tier exists for and the one with no daemon to lean on.
    let mut cache = SafeSetCache::load(root);
    let safe = cache.resolve(python, shim, root, &modules)?;
    // Best-effort: an unwritable tree must still run, just without the speedup next time.
    let _ = cache.save(root);

    let (safe_items, rest): (Vec<TestItem>, Vec<TestItem>) = items
        .into_iter()
        .partition(|it| safe.contains(&locality_key(it.node_id.as_str())));

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
    batch_must_fork: HashSet<String>,
) -> Result<Vec<TestResult>, String> {
    let BatchExec {
        strategy,
        deadline_ms,
        optimistic_no_fork,
    } = exec;
    match strategy {
        WorkerStrategy::Fork => {
            #[cfg(unix)]
            {
                // The ladder and restore are launched together or not at all — see
                // `ForkWorker::launch_optimistic`.
                let launched = if optimistic_no_fork {
                    ForkWorker::launch_optimistic(py, sh, rt)
                } else {
                    ForkWorker::launch(py, sh, rt)
                };
                let mut worker = launched
                    .map_err(|e| format!("failed to launch wellspring: {e}"))?
                    .with_deadline_ms(deadline_ms)
                    .with_trusted_pure(batch_trusted)
                    .with_must_fork(batch_must_fork);
                worker
                    .run(batch_items)
                    .map_err(|e| format!("execution failed: {e}"))
            }
            #[cfg(not(unix))]
            {
                // The optimistic ladder and the trusted-pure set are fork-only knobs; name them here
                // so this arm consumes them on platforms where the fork branch is compiled out.
                let _ = (optimistic_no_fork, batch_trusted, batch_must_fork);
                Err("fork is unavailable on this platform".to_string())
            }
        }
        // The no-fork path always snapshots/restores (its only isolation without COW); the fork-only
        // knobs (optimistic ladder, trusted-pure bare no-fork) do not apply. One process per batch.
        WorkerStrategy::Subprocess => {
            // Nothing to demote to: this tier runs in-process by configuration, not by guess.
            let _ = batch_must_fork;
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
        use crate::domain::{NodeId, ScopePath, TestItem, TestStyle};
        let plan = RunPlan {
            strategy: WorkerStrategy::Fork,
            ..RunPlan::default()
        };
        let items = vec![TestItem::new(
            NodeId::new("t.py::a"),
            TestStyle::Function,
            ScopePath::module("t.py"),
        )];
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
