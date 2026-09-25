use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
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

/// Schedule `items` into work units and drain them through a pool of `workers` threads (TID-52).
///
/// The unit granularity is the scheduler's choice: the locality scheduler yields one unit per module
/// (heaviest first), the round-robin baseline one per worker, which is the static partition this
/// used to do unconditionally. A worker that finishes its unit takes the next one instead of idling.
///
/// That distinction is worth most of the gap against pytest-xdist. A static partition has to predict
/// each worker's total cost up front, and on a cold run the only weight available is one-per-test —
/// so on pirn-agents, whose per-test cost spans four orders of magnitude, bins balanced by test
/// count ran 121/97/66/34/31/25/23/19 seconds: the machine 57% idle, and 2.32x the makespan a
/// perfectly balanced run would take. Nothing about the execution tier was wrong; the prediction was.
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

    // node id -> item, to rebuild each unit's TestItems from the scheduler's NodeId batches.
    let mut by_node: HashMap<String, TestItem> = items
        .iter()
        .map(|i| (i.node_id.to_string(), i.clone()))
        .collect();
    // Weight each collected item by what it cost last time (TID-62), or 1 on a cold run. The cold
    // weight is why the assignment must not be static (TID-52): one-per-test says nothing about a
    // suite whose per-test cost spans four orders of magnitude. The recorded weight is what turns
    // the queue's order from "most tests first" into "most time first", which is what keeps a heavy
    // module off the tail of the run.
    let recorded = RecordedWeights::new(&plan.durations);
    let scheduled: Vec<ScheduledTest> = items
        .iter()
        .map(|i| {
            ScheduledTest::new(
                i.node_id.clone(),
                locality_key(i.node_id.as_str()),
                recorded.weight_of(i.node_id.as_str()),
            )
        })
        .collect();
    let units = plan
        .scheduler
        .build()
        .units(&ScheduleInput::new(scheduled, workers));

    // The queue. `units` come heaviest first and `pop` takes from the back, so the list is built
    // reversed once here rather than searched on every take. Handing out the heaviest unit first is
    // what keeps a long module off the end of the run: started last, it *is* the tail.
    let pending: Vec<Vec<TestItem>> = units
        .iter()
        .rev()
        .map(|u| {
            u.items()
                .iter()
                .filter_map(|n| by_node.remove(n.as_str()))
                .collect::<Vec<TestItem>>()
        })
        .filter(|u| !u.is_empty())
        .collect();
    if pending.is_empty() {
        return Ok(Vec::new());
    }
    // Never more threads than units: a thread with nothing to take is a worker process launched for
    // nothing. Known before anything is forked, which is why the pool is sized from it below rather
    // than from the requested worker count.
    let threads = workers.min(pending.len());
    let queue = Arc::new(Mutex::new(pending));

    // The modules this run executes, for the shim's selective start-up (TID-75). Written to a
    // file rather than passed on the command line: a suite can name thousands of modules, and one
    // argument is capped well below that. Removed once every worker has started and finished.
    let modules_file = ModulesFile::write(&items)?;

    // TID-4: one imported image, forked per worker. Stood up before the thread loop so the import is
    // finished — and paid once — before any worker starts. Fork-tier only: the subprocess and
    // sub-interpreter tiers have no wellspring to share, by construction.
    #[cfg(unix)]
    let mut pool = if plan.shared_import && matches!(strategy, WorkerStrategy::Fork) {
        // Launched with restore unconditionally, exactly as `ForkWorker::launch_optimistic` does:
        // it costs nothing when the ladder is off, and it makes the unsound combination — in-process
        // execution with no snapshot — unreachable rather than merely unused.
        Some(
            WellspringPool::launch_selected(
                python,
                shim,
                root,
                true,
                threads,
                Some(&modules_file.path),
            )
            .map_err(|e| e.to_string())?,
        )
    } else {
        None
    };

    let exec = BatchExec {
        strategy,
        deadline_ms: plan.deadline_ms,
        optimistic_no_fork: plan.optimistic_no_fork,
    };
    let modules_path = modules_file.path.clone();
    // The whole run's sets, not one unit's slice: a thread now runs many units and cannot know in
    // advance which node ids it will see. Membership is what both are used for, so a larger set
    // costs a hash lookup and nothing else.
    let trusted: HashSet<String> = plan.trusted_pure.clone();
    let must_fork: HashSet<String> = plan.must_fork.clone();

    let mut handles = Vec::new();
    for _ in 0..threads {
        let (py, sh, rt) = (python.to_string(), shim.to_path_buf(), root.to_path_buf());
        let (queue, trusted, must_fork) = (queue.clone(), trusted.clone(), must_fork.clone());
        let modules_path = modules_path.clone();
        // A pooled transport is owned outright, so it moves into the thread without borrowing the
        // pool. The pool itself must outlive the threads — it is dropped after the joins below,
        // because its parent process only exits once every worker connection has closed.
        #[cfg(unix)]
        let pooled = pool.as_mut().and_then(|p| p.take_worker());
        #[cfg(not(unix))]
        let pooled: Option<()> = None;

        handles.push(thread::spawn(move || -> Result<Vec<TestResult>, String> {
            // One worker per thread, built once and reused across every unit it takes. Building it
            // per unit would trade the idle time this removes for a process launch per module.
            let mut worker: Box<dyn Worker> = {
                #[cfg(unix)]
                {
                    match pooled {
                        Some(transport) => Box::new(
                            PooledWorker::new(transport, exec.deadline_ms)
                                .with_optimistic_no_fork(exec.optimistic_no_fork)
                                .with_trusted_pure(trusted)
                                .with_must_fork(must_fork),
                        ),
                        None => new_worker(exec, &py, &sh, &rt, &modules_path, trusted, must_fork)?,
                    }
                }
                #[cfg(not(unix))]
                {
                    let _ = pooled;
                    new_worker(exec, &py, &sh, &rt, &modules_path, trusted, must_fork)?
                }
            };
            let mut mine = Vec::new();
            loop {
                let Some(unit) = queue.lock().expect("the work queue is not poisoned").pop() else {
                    return Ok(mine);
                };
                mine.extend(
                    worker
                        .run(&unit)
                        .map_err(|e| format!("execution failed: {e}"))?,
                );
            }
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

/// Build this thread's worker for the named tier, once, to be reused across every unit it takes.
///
/// Was `run_batch`, which launched a worker and ran exactly one batch on it. Under the work queue a
/// thread runs an unknown number of units, so construction is separated from execution — otherwise
/// every module would cost a fresh interpreter on the subprocess tier, which is more than the idle
/// time the queue removes.
fn new_worker(
    exec: BatchExec,
    py: &str,
    sh: &Path,
    rt: &Path,
    modules: &Path,
    trusted: HashSet<String>,
    must_fork: HashSet<String>,
) -> Result<Box<dyn Worker>, String> {
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
                let launched =
                    ForkWorker::launch_selected(py, sh, rt, optimistic_no_fork, Some(modules));
                Ok(Box::new(
                    launched
                        .map_err(|e| format!("failed to launch wellspring: {e}"))?
                        .with_deadline_ms(deadline_ms)
                        .with_trusted_pure(trusted)
                        .with_must_fork(must_fork),
                ))
            }
            #[cfg(not(unix))]
            {
                // The optimistic ladder and the trusted-pure set are fork-only knobs; name them here
                // so this arm consumes them on platforms where the fork branch is compiled out.
                let _ = (optimistic_no_fork, trusted, must_fork, modules);
                Err("fork is unavailable on this platform".to_string())
            }
        }
        // The no-fork path always snapshots/restores (its only isolation without COW); the fork-only
        // knobs (optimistic ladder, trusted-pure bare no-fork) do not apply.
        WorkerStrategy::Subprocess => {
            // Nothing to demote to: this tier runs in-process by configuration, not by guess.
            let _ = (must_fork, trusted);
            Ok(Box::new(
                SubprocessWorker::new(deadline_ms, 1)
                    .with_target(py, sh, rt)
                    .with_modules(modules),
            ))
        }
        // Routed before batching; reaching here would mean a nested pool.
        WorkerStrategy::SubInterp => {
            Err("the subinterp tier is routed before batching, not per batch".to_string())
        }
    }
}

/// Recorded durations, indexed so a *collected* item can be charged for every node it expands into.
///
/// The scheduler packs collected items — what the regex collector found — but durations are
/// recorded against the ids results *report*, and those differ whenever the engine expands a node
/// at runtime: a parametrized test reports one id per case (`mod.py::test_x[3-b]`), an inherited
/// class one per method (`mod.py::Class::test_y`). Charging the collected item the sum of its cases
/// is what makes a 40-case test weigh like 40 tests rather than one, which on pirn-agents is the
/// difference between 4,019 collected items and 4,657 reported nodes all weighing 1.
///
/// A `BTreeMap` so each lookup is one range scan from the item's id, not a pass over every record.
struct RecordedWeights<'a> {
    by_id: std::collections::BTreeMap<&'a str, u64>,
}

impl<'a> RecordedWeights<'a> {
    fn new(durations: &'a HashMap<String, u64>) -> Self {
        Self {
            by_id: durations.iter().map(|(k, v)| (k.as_str(), *v)).collect(),
        }
    }

    /// The item's own recorded duration plus that of every node expanded from it; `1` when nothing
    /// was recorded, so a cold item still counts and a measured-0ms one still sorts.
    fn weight_of(&self, item: &str) -> u64 {
        let total: u64 = self
            .by_id
            .range(item..)
            .take_while(|(id, _)| id.starts_with(item))
            .filter(|(id, _)| {
                // `item` itself, or an expansion of it — never a sibling that merely shares a
                // prefix (`test_a` must not be charged for `test_ab`).
                id.len() == item.len() || matches!(id.as_bytes()[item.len()], b'[' | b':')
            })
            .map(|(_, ms)| *ms)
            .sum();
        total.max(1)
    }
}

/// The file naming the modules a run executes, handed to every worker's start-up (TID-75).
///
/// Removed on drop, which is after every worker thread has been joined: a worker reads it when it
/// starts, and the last thread may start after the first has already finished.
struct ModulesFile {
    path: PathBuf,
}

impl ModulesFile {
    fn write(items: &[TestItem]) -> Result<Self, String> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let mut modules: Vec<String> = items
            .iter()
            .map(|i| locality_key(i.node_id.as_str()))
            .collect();
        modules.sort();
        modules.dedup();
        let path = std::env::temp_dir().join(format!(
            "tiderace-modules-{}-{}.txt",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::write(&path, modules.join("\n") + "\n")
            .map_err(|e| format!("could not write the module selection: {e}"))?;
        Ok(Self { path })
    }
}

impl Drop for ModulesFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// A test's locality key for scheduling — its module (the file part of the node id), so a module's
/// tests co-locate on one worker and reuse its module/session snapshot.
pub fn locality_key(node_id: &str) -> String {
    node_id.split("::").next().unwrap_or(node_id).to_string()
}

#[cfg(test)]
mod tests {
    use super::{locality_key, run_parallel, RecordedWeights};
    use crate::runner::RunPlan;
    #[cfg(not(unix))]
    use crate::runner::WorkerStrategy;
    use std::collections::HashMap;
    use std::path::Path;

    #[test]
    fn a_collected_item_is_charged_for_every_node_it_expanded_into() {
        let durations: HashMap<String, u64> = [
            ("m.py::test_a", 5),
            ("m.py::test_a[1]", 100),
            ("m.py::test_a[2]", 200),
            ("m.py::test_ab", 1_000), // shares a prefix; is not an expansion
            ("m.py::Klass::test_x", 40),
            ("m.py::Klass::test_y", 60),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
        let w = RecordedWeights::new(&durations);
        assert_eq!(w.weight_of("m.py::test_a"), 305, "own time plus both cases");
        assert_eq!(
            w.weight_of("m.py::test_ab"),
            1_000,
            "a sibling is not a case"
        );
        assert_eq!(
            w.weight_of("m.py::Klass"),
            100,
            "an inherited class is charged for the methods it expanded into"
        );
        assert_eq!(w.weight_of("m.py::test_unknown"), 1, "cold items weigh 1");
    }

    #[test]
    fn a_zero_recording_still_weighs_one() {
        let durations: HashMap<String, u64> = [("m.py::t".to_string(), 0)].into_iter().collect();
        assert_eq!(RecordedWeights::new(&durations).weight_of("m.py::t"), 1);
    }

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
