//! `MemoryGovernor` (W13) — bounds concurrent forks by an **RSS budget**, not just CPU count, to
//! prevent COW write-amplification OOM under fixture-heavy suites (design 05 §6.3).
//!
//! `max_concurrent_forks() = min(cpu, rss_budget / per_fork_estimate)`, where `per_fork_estimate`
//! is **seeded from `Watermark.rss_bytes`** of the fork-from layer and refined by observed child
//! RSS over the run. `admit()` blocks until the budget allows another in-flight child.
//!
//! **Contract seam.** Struct shape + method signatures frozen here; the admission/refinement logic
//! is implemented by Lane FALLBACK (subagent fb-governor).

use crate::error::Result;
use crate::exec::fork_permit::ForkPermit;

/// RSS-budget admission controller for concurrent forks.
// Fields are written by `new` and read by the admission math Lane FALLBACK (fb-governor) fills in;
// allowed dead until that scaffold is replaced (W13).
#[allow(dead_code)]
#[derive(Debug)]
pub struct MemoryGovernor {
    /// Total RSS budget for in-flight forks (bytes).
    rss_budget: u64,
    /// Current per-fork RSS estimate (bytes); seeded from `Watermark.rss_bytes`, refined at runtime.
    per_fork_estimate: u64,
    /// CPU-count ceiling (the other input to `min(...)`).
    cpu_limit: usize,
}

impl MemoryGovernor {
    /// Construct a governor with a budget, an initial per-fork estimate (the fork-from layer's
    /// `rss_bytes`), and a CPU ceiling.
    ///
    /// Defined (trivial field init) so lanes can construct one without a scaffold.
    pub fn new(rss_budget: u64, per_fork_estimate: u64, cpu_limit: usize) -> Self {
        Self {
            rss_budget,
            per_fork_estimate,
            cpu_limit,
        }
    }

    /// `min(cpu, rss_budget / per_fork_estimate)`, clamped to ≥ 1 (always make progress, even when
    /// `rss_budget < per_fork_estimate`) (W13).
    ///
    /// LANE: Lane FALLBACK (fb-governor) implements max_concurrent_forks — W13.
    pub fn max_concurrent_forks(&self) -> usize {
        unimplemented!(
            "LANE: Lane FALLBACK (fb-governor) implements MemoryGovernor::max_concurrent_forks — W13"
        )
    }

    /// Block until the budget admits another in-flight fork; return its [`ForkPermit`] (W13).
    ///
    /// LANE: Lane FALLBACK (fb-governor) implements admit — W13.
    pub fn admit(&self) -> Result<ForkPermit> {
        unimplemented!("LANE: Lane FALLBACK (fb-governor) implements MemoryGovernor::admit — W13")
    }

    /// Refine `per_fork_estimate` from an observed child RSS sample (W13).
    ///
    /// LANE: Lane FALLBACK (fb-governor) implements observe_rss — W13.
    pub fn observe_rss(&mut self, _observed_bytes: u64) {
        unimplemented!(
            "LANE: Lane FALLBACK (fb-governor) implements MemoryGovernor::observe_rss — W13"
        )
    }
}
