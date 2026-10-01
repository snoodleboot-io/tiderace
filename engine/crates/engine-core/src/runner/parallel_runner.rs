use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;

use crate::domain::{NodeId, TestItem, TestResult};
use crate::error::{EngineError, Result};
use crate::exec::RunKnobs;
#[cfg(unix)]
use crate::exec::{ForkWorker, PooledWorker, WellspringPool};
use crate::exec::{SafeSetCache, ShimTarget, SubInterpWorker, SubprocessWorker, Worker};
use crate::runner::{Learned, RunNotes, RunOutcome, RunPlan, Sharding, WorkerStrategy};
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
    learned: &Learned,
) -> Result<Vec<TestResult>> {
    run_parallel_with_notes(python, shim, root, items, plan, learned).map(|o| o.results)
}

/// [`run_parallel`], with the notes the run collected on the way — the memory sizing it
/// applied, a cache it could not save — for the caller to print or keep (TID-115).
pub fn run_parallel_with_notes(
    python: &str,
    shim: &Path,
    root: &Path,
    items: Vec<TestItem>,
    plan: &RunPlan,
    learned: &Learned,
) -> Result<RunOutcome> {
    if items.is_empty() {
        return Ok(RunOutcome::default());
    }
    if !plan.strategy.is_available() {
        return Err(EngineError::Unavailable(format!(
            "the {} tier is not available on this platform",
            plan.strategy
        )));
    }
    let mut notes = RunNotes::default();
    let results = if plan.strategy.is_hybrid() {
        run_subinterp_hybrid(
            &ShimTarget::new(python, shim, root),
            items,
            plan,
            learned,
            &mut notes,
        )?
    } else {
        run_batched(
            &ShimTarget::new(python, shim, root),
            items,
            plan,
            learned,
            plan.strategy,
            None,
            &mut notes,
        )?
    };
    Ok(RunOutcome { results, notes })
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
/// As [`run_parallel`], drawing this run's workers from a **warm** pool (TID-84): a persistent
/// parent that already holds the imported suite forks a fresh set for the run, so nothing is
/// imported. The caller owns the pool and decides when its image is stale.
#[cfg(unix)]
pub fn run_parallel_with_pool(
    python: &str,
    shim: &Path,
    root: &Path,
    items: Vec<TestItem>,
    plan: &RunPlan,
    learned: &Learned,
    pool: &mut WellspringPool,
) -> Result<Vec<TestResult>> {
    run_parallel_with_pool_notes(python, shim, root, items, plan, learned, pool).map(|o| o.results)
}

/// [`run_parallel_with_pool`], with the run's notes (TID-115).
#[cfg(unix)]
pub fn run_parallel_with_pool_notes(
    python: &str,
    shim: &Path,
    root: &Path,
    items: Vec<TestItem>,
    plan: &RunPlan,
    learned: &Learned,
    pool: &mut WellspringPool,
) -> Result<RunOutcome> {
    let mut notes = RunNotes::default();
    let results = run_batched(
        &ShimTarget::new(python, shim, root),
        items,
        plan,
        learned,
        WorkerStrategy::Fork,
        Some(pool),
        &mut notes,
    )?;
    Ok(RunOutcome { results, notes })
}

fn run_batched(
    target: &ShimTarget,
    items: Vec<TestItem>,
    plan: &RunPlan,
    learned: &Learned,
    strategy: WorkerStrategy,
    #[cfg(unix)] warm: Option<&mut WellspringPool>,
    #[cfg(not(unix))] warm: Option<()>,
    notes: &mut RunNotes,
) -> Result<Vec<TestResult>> {
    #[cfg(not(unix))]
    let _ = (warm, &notes); // the pool, and the notes its sizing writes, are Unix-only
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
    let recorded = RecordedWeights::new(&learned.durations);
    let scheduled: Vec<ScheduledTest> = items
        .iter()
        .map(|i| {
            ScheduledTest::new(
                i.node_id.clone(),
                i.node_id.file().to_string(),
                recorded.weight_of(i.node_id.as_str()),
            )
        })
        .collect();
    let units = plan.scheduler.build().units(
        &ScheduleInput::new(scheduled, workers)
            .with_module_sharding(plan.sharding == Sharding::SplitModules),
    );

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
    let mut owned_pool =
        if warm.is_none() && plan.fork.shared_import && matches!(strategy, WorkerStrategy::Fork) {
            // Launched with restore unconditionally, exactly as `ForkWorker::launch_optimistic` does:
            // it costs nothing when the ladder is off, and it makes the unsound combination — in-process
            // execution with no snapshot — unreachable rather than merely unused.
            // Persistent rather than sized at launch (TID-106): the image is up, and its resident
            // size known, before the run decides how many workers to fork off it.
            Some(WellspringPool::launch_persistent_selected(
                &target.python,
                &target.shim,
                &target.root,
                true,
                Some(&modules_file.path),
            )?)
        } else {
            None
        };
    // The persistent parent — warm (TID-84) or this run's own — forks the workers now, off its
    // imported image; how many is the thread count, capped by what memory allows (TID-106).
    #[cfg(unix)]
    let mut threads = threads;
    #[cfg(unix)]
    let mut pool: Option<&mut WellspringPool> = match warm {
        Some(w) => Some(w),
        None => owned_pool.as_mut(),
    };
    #[cfg(unix)]
    if let Some(p) = pool.as_deref_mut() {
        let sizing = super::memory::workers_by_memory(
            threads,
            plan.workers.is_explicit(),
            super::memory::process_rss_bytes(p.pid()),
            super::memory::available_memory_bytes(),
            plan.memory_limit_mb.map(|mb| mb << 20),
        );
        if let Some(note) = &sizing.note {
            notes.push(note.clone());
        }
        threads = sizing.workers;
        p.spawn_workers(threads)?;
    }

    // The whole run's sets, not one unit's slice: a thread runs many units and cannot know in
    // advance which node ids it will see. Shared by `Arc`, so a thread costs a pointer, not a copy.
    let exec = BatchExec {
        strategy,
        knobs: RunKnobs::new(plan.deadline_ms)
            .with_optimistic_no_fork(plan.fork.ladder)
            .with_trusted_pure(learned.trusted_pure.clone())
            .with_must_fork(learned.must_fork.clone()),
    };
    let modules_path = modules_file.path.clone();

    // The run's clock for the schedule stamps (TID-78): every unit's start and end is measured
    // from here, so a report can be drawn as one lane per worker.
    let run_started = std::time::Instant::now();
    // Units are numbered in the order they are taken — heaviest first, so unit 0 is the schedule's
    // first pick and the number reads as its rank.
    let unit_counter = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut handles = Vec::new();
    for worker_index in 0..threads {
        let target = target.clone();
        let exec = exec.clone();
        let queue = queue.clone();
        let unit_counter = unit_counter.clone();
        let modules_path = modules_path.clone();
        // A pooled transport is owned outright, so it moves into the thread without borrowing the
        // pool. The pool itself must outlive the threads — it is dropped after the joins below,
        // because its parent process only exits once every worker connection has closed.
        #[cfg(unix)]
        let pooled = pool.as_deref_mut().and_then(|p| p.take_worker());
        #[cfg(not(unix))]
        let pooled: Option<()> = None;

        handles.push(thread::spawn(move || -> Result<Vec<TestResult>> {
            // One worker per thread, built once and reused across every unit it takes. Building it
            // per unit would trade the idle time this removes for a process launch per module.
            let mut worker: Box<dyn Worker> = {
                #[cfg(unix)]
                {
                    match pooled {
                        Some(transport) => Box::new(
                            PooledWorker::new(transport, exec.knobs.deadline_ms)
                                .with_knobs(exec.knobs),
                        ),
                        None => new_worker(exec, &target, &modules_path)?,
                    }
                }
                #[cfg(not(unix))]
                {
                    let _ = pooled;
                    new_worker(exec, &target, &modules_path)?
                }
            };
            let mut mine = Vec::new();
            // The worker process's peak resident size over the run, sampled after every unit
            // (TID-106); stamped on its results on the way out.
            let mut peak_rss: u64 = 0;
            let finish = |mine: Vec<TestResult>, peak_rss: u64| -> Vec<TestResult> {
                let mb = (peak_rss > 0).then_some(peak_rss >> 20);
                mine.into_iter()
                    .map(|r| r.with_worker_peak_rss_mb(mb))
                    .collect()
            };
            loop {
                let Some(unit) = queue.lock().expect("the work queue is not poisoned").pop() else {
                    return Ok(finish(mine, peak_rss));
                };
                let unit_index = unit_counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let started_ms = run_started.elapsed().as_millis() as u64;
                let results = worker.run(&unit)?;
                let ended_ms = run_started.elapsed().as_millis() as u64;
                mine.extend(
                    results
                        .into_iter()
                        .map(|r| r.with_schedule(worker_index, unit_index, started_ms, ended_ms)),
                );
                if let Some(rss) = worker.pid().and_then(super::memory::process_rss_bytes) {
                    peak_rss = peak_rss.max(rss);
                }
                if worker.is_lost() {
                    // Its last unit is reported; the queue drains on the other workers (TID-93).
                    return Ok(finish(mine, peak_rss));
                }
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
                first_err
                    .get_or_insert_with(|| EngineError::Exec("worker thread panicked".to_string()));
            }
        }
    }
    // Every worker connection is closed by now (the threads owned them), so a pool this run owns
    // can exit. Dropping it here rather than on the `?` path above is what keeps a failing run
    // from leaving an orphaned parent behind holding the imported image. A borrowed (warm) pool
    // stays with its owner: that is the image the next run forks from.
    #[cfg(unix)]
    drop(owned_pool.take());
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
    target: &ShimTarget,
    items: Vec<TestItem>,
    plan: &RunPlan,
    learned: &Learned,
    notes: &mut RunNotes,
) -> Result<Vec<TestResult>> {
    let mut modules: Vec<String> = items.iter().map(|i| i.node_id.file().to_string()).collect();
    modules.sort();
    modules.dedup();

    // Probing means launching a fresh interpreter per module, so it is cached by content hash and
    // only new or changed modules pay (TID-35). Without this the CLI re-probed the whole corpus on
    // every invocation, which on a small module count is most of this tier's cost — and it hurt
    // most on Windows, the one platform the tier exists for and the one with no daemon to lean on.
    let mut cache = SafeSetCache::load(&target.root);
    let safe = cache
        .resolve(&target.python, &target.shim, &target.root, &modules)
        .map_err(EngineError::Exec)?;
    // Best-effort: an unwritable tree must still run, just without the speedup next time — but
    // say so, or the re-probe on every run looks like the tier being slow.
    if let Err(e) = cache.save(&target.root) {
        notes.push(format!("sub-interpreter safe-set cache not saved: {e}"));
    }

    let (safe_items, rest): (Vec<TestItem>, Vec<TestItem>) = items
        .into_iter()
        .partition(|it| safe.contains(it.node_id.file()));

    let mut all = Vec::new();
    if !safe_items.is_empty() {
        let mut worker = SubInterpWorker::new(plan.deadline_ms)
            .with_shim_target(target.clone())
            .with_pool_size(plan.effective_workers(safe_items.len()));
        all.extend(worker.run(&safe_items)?);
    }
    if !rest.is_empty() {
        all.extend(run_batched(
            target,
            rest,
            plan,
            learned,
            plan.strategy.fallback(),
            None,
            notes,
        )?);
    }
    Ok(all)
}

/// The per-batch execution settings, split out so a batch can be handed across a thread boundary
/// as one value instead of a fistful of positional scalars.
#[derive(Debug, Clone)]
struct BatchExec {
    strategy: WorkerStrategy,
    knobs: RunKnobs,
}

/// Build this thread's worker for the named tier, once, to be reused across every unit it takes.
///
/// Was `run_batch`, which launched a worker and ran exactly one batch on it. Under the work queue a
/// thread runs an unknown number of units, so construction is separated from execution — otherwise
/// every module would cost a fresh interpreter on the subprocess tier, which is more than the idle
/// time the queue removes.
fn new_worker(exec: BatchExec, target: &ShimTarget, modules: &Path) -> Result<Box<dyn Worker>> {
    let BatchExec { strategy, knobs } = exec;
    match strategy {
        WorkerStrategy::Fork => {
            #[cfg(unix)]
            {
                // The ladder and restore are launched together or not at all — see
                // `ForkWorker::launch_optimistic`.
                Ok(Box::new(
                    ForkWorker::launch_target(
                        target,
                        knobs.optimistic_no_fork,
                        Some(modules),
                        knobs.deadline_ms,
                    )?
                    .with_knobs(knobs),
                ))
            }
            #[cfg(not(unix))]
            {
                let _ = modules;
                Err(EngineError::Unavailable(
                    "fork is unavailable on this platform".to_string(),
                ))
            }
        }
        // The no-fork path always snapshots/restores (its only isolation without COW); the fork-only
        // knobs (optimistic ladder, trusted-pure bare no-fork) do not apply.
        WorkerStrategy::Subprocess => Ok(Box::new(
            SubprocessWorker::new(knobs.deadline_ms, 1)
                .with_shim_target(target.clone())
                .with_modules(modules),
        )),
        // Routed before batching; reaching here would mean a nested pool.
        WorkerStrategy::SubInterp => Err(EngineError::Unavailable(
            "the subinterp tier is routed before batching, not per batch".to_string(),
        )),
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
    fn new(durations: &'a HashMap<NodeId, u64>) -> Self {
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
    fn write(items: &[TestItem]) -> Result<Self> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let mut modules: Vec<String> = items.iter().map(|i| i.node_id.file().to_string()).collect();
        modules.sort();
        modules.dedup();
        let path = std::env::temp_dir().join(format!(
            "tiderace-modules-{}-{}.txt",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::write(&path, modules.join("\n") + "\n")?;
        Ok(Self { path })
    }
}

impl Drop for ModulesFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::{run_parallel, RecordedWeights};
    use crate::domain::NodeId;
    #[cfg(not(unix))]
    use crate::runner::WorkerStrategy;
    use crate::runner::{Learned, RunPlan};
    use std::collections::HashMap;
    use std::path::Path;

    #[test]
    fn a_collected_item_is_charged_for_every_node_it_expanded_into() {
        let durations: HashMap<NodeId, u64> = [
            ("m.py::test_a", 5),
            ("m.py::test_a[1]", 100),
            ("m.py::test_a[2]", 200),
            ("m.py::test_ab", 1_000), // shares a prefix; is not an expansion
            ("m.py::Klass::test_x", 40),
            ("m.py::Klass::test_y", 60),
        ]
        .into_iter()
        .map(|(k, v)| (NodeId::new(k), v))
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
        let durations: HashMap<NodeId, u64> = [(NodeId::new("m.py::t"), 0)].into_iter().collect();
        assert_eq!(RecordedWeights::new(&durations).weight_of("m.py::t"), 1);
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
            &Learned::default(),
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
            &Learned::default(),
        )
        .expect_err("fork must be refused where it does not exist");
        assert!(
            err.to_string().contains("fork"),
            "message must name the tier; got {err:?}"
        );
    }
}
