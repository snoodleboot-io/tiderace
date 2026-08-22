use std::collections::HashSet;

use crate::runner::{SchedulerKind, WorkerStrategy};

/// A sensible default worker count: the machine's parallelism, falling back to 4.
pub fn default_workers() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
}

/// The default per-test deadline, in milliseconds.
///
/// A deadline exists to catch a **hang**, not to enforce speed, and 5s was doing the latter: pytest
/// ships no per-test timeout at all (`pytest-timeout` is opt-in), and suites that do set one
/// conventionally pick 60s — `pirn-agents` runs its own CI with `pytest --timeout=60`. At 5s a test
/// that legitimately shells out to a fresh interpreter failed here and passed under pytest, which is
/// a wrong red arriving through configuration rather than logic (TID-28).
///
/// The trade is asymmetric. A looser default costs one worker sitting on a genuine hang for 60s
/// instead of 5; a tighter one costs false errors on every suite that spawns a subprocess, which is
/// many. `--timeout` overrides either way.
pub const DEFAULT_DEADLINE_MS: u64 = 60_000;

/// Compile-time floor on the above. Tuning stays possible, but lowering it back into the seconds
/// range has to confront the reasoning: a subprocess-spawning test on a large-import corpus needs an
/// interpreter start plus the project import (~2.6s measured) before it does any work.
const _: () = assert!(
    DEFAULT_DEADLINE_MS >= 30_000,
    "the default deadline must not be tight enough to fail slow-but-valid tests (TID-28)"
);

/// Everything a run needs to know about *how* to execute, separate from *what* to execute (TID-17).
///
/// Bundled into one value so a run can state its own configuration — the missing half of every
/// benchmark taken through the old CLI, which measured one combination and reported it unqualified.
#[derive(Debug, Clone)]
pub struct RunPlan {
    /// Which isolation tier executes each batch.
    pub strategy: WorkerStrategy,
    /// How the corpus is partitioned across workers.
    pub scheduler: SchedulerKind,
    /// How many workers to run in parallel. Clamped to at least 1, and never more than the test count.
    pub workers: usize,
    /// Per-test deadline in milliseconds.
    pub deadline_ms: u64,
    /// Whether the fork tier may take the optimistic in-process ladder for pure tests.
    ///
    /// **Off by default.** Turned on in TID-23 on a measurement that looked clean, then turned back
    /// off when TID-26 uncovered 129 previously-uncollected tests and four of them failed under the
    /// ladder while passing under fork.
    ///
    /// The cause is a real limit, not a bug to patch out: `_snapshot_shared` snapshots the **test
    /// module's** globals only. A test that mutates state owned by a *library* module — registering
    /// into a registry, installing a prompt pack — has nothing restored, and the next test sees it.
    /// Fork has no such hole, because the child is a whole pristine process.
    ///
    /// Worth ~2.4x when it is safe (24.0s -> 10.2s on a real corpus), so it stays available behind
    /// `--optimistic`; it is not sound enough to be the default.
    pub optimistic_no_fork: bool,
    /// Node ids recorded pure, eligible for the bare no-fork tier (TID-1).
    pub trusted_pure: HashSet<String>,
}

impl Default for RunPlan {
    fn default() -> Self {
        Self {
            strategy: WorkerStrategy::platform_default(),
            scheduler: SchedulerKind::default(),
            workers: default_workers(),
            deadline_ms: DEFAULT_DEADLINE_MS,
            optimistic_no_fork: false,
            trusted_pure: HashSet::new(),
        }
    }
}

impl RunPlan {
    /// A one-line description of the configuration, for the run header.
    ///
    /// The acceptance criterion for TID-17 is that a run says which tiers it used, so this is the
    /// deliverable as much as the flags are: a pasted benchmark number is uninterpretable without it.
    pub fn header(&self) -> String {
        let mut s = format!(
            "strategy={} scheduler={} workers={} timeout={}ms",
            self.strategy, self.scheduler, self.workers, self.deadline_ms
        );
        if self.strategy.is_hybrid() {
            // Say so explicitly: a `subinterp` run that quietly forked most of the corpus, reported
            // as "sub-interpreter performance", is exactly the confusion this ticket exists about.
            s.push_str(&format!(
                " (safe subset; rest via {})",
                self.strategy.fallback()
            ));
        }
        if self.optimistic_no_fork {
            s.push_str(" optimistic-no-fork");
        }
        s
    }

    /// Clamp the worker count against the real test count — N workers for fewer than N tests just
    /// pays launch cost for idle wellsprings.
    pub fn effective_workers(&self, test_count: usize) -> usize {
        self.workers.max(1).min(test_count.max(1))
    }
}

#[cfg(test)]
mod tests {
    use super::{default_workers, RunPlan, DEFAULT_DEADLINE_MS};
    use crate::runner::{SchedulerKind, WorkerStrategy};

    #[test]
    fn default_plan_is_runnable_on_this_platform() {
        let plan = RunPlan::default();
        assert!(plan.strategy.is_available());
        assert!(plan.workers >= 1);
        assert_eq!(plan.deadline_ms, DEFAULT_DEADLINE_MS);
    }

    #[test]
    fn default_workers_is_at_least_one() {
        assert!(default_workers() >= 1);
    }

    #[test]
    fn header_names_every_knob() {
        let plan = RunPlan {
            strategy: WorkerStrategy::Subprocess,
            scheduler: SchedulerKind::RoundRobin,
            workers: 3,
            deadline_ms: 1234,
            ..RunPlan::default()
        };
        let h = plan.header();
        for expected in ["subprocess", "round-robin", "workers=3", "1234ms"] {
            assert!(
                h.contains(expected),
                "header {h:?} must mention {expected:?}"
            );
        }
    }

    #[test]
    fn hybrid_header_discloses_the_fallback() {
        let plan = RunPlan {
            strategy: WorkerStrategy::SubInterp,
            ..RunPlan::default()
        };
        let h = plan.header();
        assert!(
            h.contains("safe subset"),
            "a hybrid run must not read as if the whole corpus used the tier; got {h:?}"
        );
        assert!(h.contains(&WorkerStrategy::SubInterp.fallback().to_string()));
    }

    #[test]
    fn the_optimistic_ladder_is_off_by_default_and_visible_when_on() {
        // Off because restore only covers the test module's own globals, so a test that mutates a
        // library module's state leaks into the next one (found via TID-26).
        assert!(!RunPlan::default().optimistic_no_fork);
        let plan = RunPlan {
            optimistic_no_fork: true,
            ..RunPlan::default()
        };
        assert!(plan.header().contains("optimistic-no-fork"));
    }

    #[test]
    fn effective_workers_never_exceeds_the_test_count_and_never_hits_zero() {
        let plan = RunPlan {
            workers: 16,
            ..RunPlan::default()
        };
        assert_eq!(plan.effective_workers(3), 3);
        assert_eq!(plan.effective_workers(64), 16);
        // An empty corpus must still yield a usable count rather than 0.
        assert_eq!(plan.effective_workers(0), 1);

        let degenerate = RunPlan {
            workers: 0,
            ..RunPlan::default()
        };
        assert_eq!(degenerate.effective_workers(10), 1);
    }
}
