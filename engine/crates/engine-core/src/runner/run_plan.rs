use std::collections::HashSet;

use crate::runner::{SchedulerKind, WorkerStrategy};

/// A sensible default worker count: the machine's parallelism, falling back to 4.
pub fn default_workers() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
}

/// The default per-test deadline, in milliseconds.
pub const DEFAULT_DEADLINE_MS: u64 = 5_000;

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
    /// **Off by default.** The ladder's safety net is snapshot/restore, and restore currently rebinds
    /// module globals rather than restoring them in place (TID-22), so a test whose state something
    /// else holds a reference to sees the two diverge. Until that lands, forking every test is the
    /// configuration that is actually correct, and speed is not worth a wrong green.
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
        // Off by default while TID-22 is open: restore rebinds rather than restores in place, so the
        // ladder's safety net is not sound yet.
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
