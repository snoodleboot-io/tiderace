use std::path::Path;

use crate::domain::{TestItem, TestResult};
use crate::error::{EngineError, Result};
use crate::exec::{RunKnobs, ShimTarget, WarmImage, WorkerStrategy};
use crate::runner::schedule::{units, ModulesFile};
use crate::runner::{lane, Learned, RunNotes, RunOutcome, RunPlan};

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
    run_with(
        &ShimTarget::new(python, shim, root),
        items,
        plan,
        learned,
        WarmImage::none(),
    )
}

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
    pool: &mut crate::exec::WellspringPool,
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
    pool: &mut crate::exec::WellspringPool,
) -> Result<RunOutcome> {
    run_with(
        &ShimTarget::new(python, shim, root),
        items,
        plan,
        learned,
        WarmImage::of(pool),
    )
}

/// [`run_parallel_with_notes`], forking this run's workers off `warm` when it holds an image
/// (TID-84). The handle is a plain type on every platform, so a caller that may or may not hold
/// an image — the daemon — carries it without a `cfg` of its own.
pub fn run_parallel_warm_notes(
    python: &str,
    shim: &Path,
    root: &Path,
    items: Vec<TestItem>,
    plan: &RunPlan,
    learned: &Learned,
    warm: WarmImage<'_>,
) -> Result<RunOutcome> {
    run_with(
        &ShimTarget::new(python, shim, root),
        items,
        plan,
        learned,
        warm,
    )
}

/// The run: the tier claims what it runs itself, the scheduler partitions the rest into units,
/// the tier prepares for the lane count, and one lane per thread drains the queue. The runner
/// knows the tier only as a [`TierFactory`](crate::exec::TierFactory); which tier, and what it
/// does on which platform, is the tier's own business (TID-118).
fn run_with(
    target: &ShimTarget,
    items: Vec<TestItem>,
    plan: &RunPlan,
    learned: &Learned,
    warm: WarmImage<'_>,
) -> Result<RunOutcome> {
    if items.is_empty() {
        return Ok(RunOutcome::default());
    }
    // A warm image is a fork-tier image: a run drawing on one runs on the fork tier, as the
    // daemon's warm path always has.
    let strategy = if warm.is_some() {
        WorkerStrategy::Fork
    } else {
        plan.strategy
    };
    if !strategy.is_available() {
        return Err(EngineError::Unavailable(format!(
            "the {strategy} tier is not available on this platform"
        )));
    }
    let mut notes = RunNotes::default();
    // The whole run's sets, not one unit's slice: a lane runs many units and cannot know in
    // advance which node ids it will see. Shared by `Arc`, so a lane costs a pointer, not a copy.
    let knobs = RunKnobs::new(plan.deadline_ms)
        .with_optimistic_no_fork(plan.fork.ladder)
        .with_trusted_pure(learned.trusted_pure.clone())
        .with_must_fork(learned.must_fork.clone());
    let mut tier = strategy.factory(target, plan, knobs, warm)?;

    let (mut results, rest) = tier.claim(items, &mut notes)?;
    if rest.is_empty() {
        return Ok(RunOutcome { results, notes });
    }
    let workers = plan.effective_workers(rest.len());
    let queue = units(&rest, plan, learned, workers);
    if queue.is_empty() {
        return Ok(RunOutcome { results, notes });
    }
    // Never more lanes than units: a lane with nothing to take is a worker process launched for
    // nothing. Known before anything is forked, which is why the pool is sized from it rather
    // than from the requested worker count.
    let lanes = workers.min(queue.len());
    let modules = ModulesFile::write(&rest)?;
    let lanes = tier.prepare(lanes, &modules.path, &mut notes)?;
    let seeds = (0..lanes)
        .map(|index| tier.lane(index, &modules.path))
        .collect::<Result<Vec<_>>>()?;
    results.extend(lane::drain_all(seeds, queue)?);
    // Every lane is joined, so a pool this run owns can exit: the factory drops here, after the
    // workers' connections have closed, rather than on the `?` paths above — which is what keeps
    // a failing run from leaving an orphaned parent behind holding the imported image. A borrowed
    // (warm) pool stays with its owner: that is the image the next run forks from.
    drop(tier);
    Ok(RunOutcome { results, notes })
}

#[cfg(test)]
mod tests {
    use super::run_parallel;
    #[cfg(not(unix))]
    use crate::exec::WorkerStrategy;
    use crate::runner::{Learned, RunPlan};
    use std::path::Path;

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
