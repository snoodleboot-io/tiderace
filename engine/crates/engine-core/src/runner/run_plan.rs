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
    /// Whether the fork tier may take the optimistic in-process ladder for restorable tests.
    ///
    /// **On by default**, as of TID-33. It has been on before and was reverted, so the history is
    /// worth stating plainly rather than trusting the current measurement on its own:
    ///
    /// * TID-23 turned it on, on a corpus that looked clean.
    /// * TID-26 turned it back off. It had uncovered 129 tests collection was silently dropping,
    ///   and four of them failed under the ladder while passing under fork.
    /// * TID-27 fixed the specific cause of those four: `_snapshot_shared` covers the **test
    ///   module's** globals, so a test that swapped a *library* module had nothing restored.
    ///
    /// What makes this time different is not a fourth patched category. Each of those fixes closed
    /// one hole in a list nobody could show was complete, which is why the revert was right and why
    /// re-flipping on "we fixed the last one" would have been wrong again. TID-33 replaced the list
    /// with a fingerprint taken around every in-process test: a test that disturbs state nothing
    /// undid is *detected* whether or not we modelled that category, re-run in a fork so the current
    /// run reports the right answer, and recorded so later runs fork it from the start.
    ///
    /// So the ladder is no longer a bet that the list is complete. It is a bet that a cheap
    /// fingerprint notices when the world moved, which is a much smaller thing to be wrong about.
    ///
    /// It is not free of limits, and one is worth naming here: a **thread** a test leaves running
    /// cannot be unwound. That is detected and the node is demoted, but a neighbour that counts
    /// threads will still see it. Fork has no such hole, because the child is a whole pristine
    /// process — `--no-optimistic` (or `TIDERACE_FORCE_FORK=1`) is the way back to it.
    ///
    /// Measured on a 4,514-test corpus: outcomes identical to fork and to pytest, zero fingerprint
    /// trips, 2.18x faster than pytest against fork-per-test's 1.12x — and, the part wall clock
    /// hides, 69s of CPU against fork-per-test's 140s for the same work.
    pub optimistic_no_fork: bool,
    /// Node ids recorded pure, eligible for the bare no-fork tier (TID-1).
    pub trusted_pure: HashSet<String>,
    /// Import the project **once** and fork the workers from that image, instead of running N
    /// independent wellsprings that each import it (TID-4).
    ///
    /// **On by default.** Shipped opt-in first, on the reasoning that it was a new fork topology in
    /// the engine's most correctness-critical path and bought efficiency rather than latency — so
    /// there was nothing to trade soak time against. Two of those three premises turned out to be
    /// wrong, which is why the default moved:
    ///
    /// * It is not only an efficiency win. Wall clock improved 20% (19.1s → 15.2s on a 4,514-test
    ///   corpus), because eight simultaneous imports occupy the same eight cores the tests want.
    /// * The topology is less novel than it looked. Each worker is an ordinary `serve` loop that
    ///   forks per test exactly as before; the only difference is that its interpreter arrived by
    ///   `fork()` instead of by `exec()`. Every worker still builds its own `Engine` *after* the
    ///   fork, so fixture state is per-worker exactly as it was with N separate wellsprings.
    ///
    /// What it removes is paying the project's import N times. On a large-import corpus that is
    /// ~2.6s per worker against a 0.03s bare interpreter — user CPU 59.8s → 30.8s at eight workers.
    /// Invisible on a laptop with idle cores; the whole bill on a CI runner charged per core-minute.
    ///
    /// Fork-tier only, and therefore inert on Windows, where `platform_default()` is the subprocess
    /// tier. `--no-shared-import` (or `TIDERACE_NO_SHARED_IMPORT=1`) goes back to one wellspring
    /// per worker.
    pub shared_import: bool,
    /// Node ids recorded as disturbing interpreter state — forked even under the ladder (TID-33).
    ///
    /// The shim detects a first offence on its own and re-runs it forked, so correctness does not
    /// depend on this set being populated. What it buys is not paying for that discovery — a wasted
    /// in-process run plus a fork — on every subsequent run.
    pub must_fork: HashSet<String>,
}

impl Default for RunPlan {
    fn default() -> Self {
        Self {
            strategy: WorkerStrategy::platform_default(),
            scheduler: SchedulerKind::default(),
            workers: default_workers(),
            deadline_ms: DEFAULT_DEADLINE_MS,
            optimistic_no_fork: true,
            shared_import: true,
            trusted_pure: HashSet::new(),
            must_fork: HashSet::new(),
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
        // Named in both directions. It used to be printed only when on, because it was the unusual
        // choice; now that it is the default the *absence* of the ladder is the fact a pasted
        // benchmark number needs, and a run that says nothing about it is uninterpretable either way.
        s.push_str(if self.optimistic_no_fork {
            " optimistic-no-fork"
        } else {
            " fork-per-test"
        });
        // Named in both directions, like the ladder: now that it is the default, its *absence* is
        // the fact a pasted benchmark number needs.
        s.push_str(if self.shared_import {
            " shared-import"
        } else {
            " import-per-worker"
        });
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
    fn the_optimistic_ladder_is_on_by_default_and_the_header_says_which() {
        // On since TID-33: a test that disturbs state the restore cannot model is detected by the
        // fingerprint, re-run forked, and remembered — so the ladder no longer rests on our list of
        // restorable categories being complete, which is what forced the TID-26 revert.
        assert!(RunPlan::default().optimistic_no_fork);
        assert!(RunPlan::default().header().contains("optimistic-no-fork"));

        // And the header names the other direction too, so a benchmark taken with the ladder off is
        // not silently indistinguishable from one taken with it on.
        let forking = RunPlan {
            optimistic_no_fork: false,
            ..RunPlan::default()
        };
        let header = forking.header();
        assert!(header.contains("fork-per-test"), "got: {header}");
        assert!(!header.contains("optimistic-no-fork"), "got: {header}");
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
