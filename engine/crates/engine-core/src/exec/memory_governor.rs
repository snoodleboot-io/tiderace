//! `MemoryGovernor` (W13) — bounds concurrent forks by an **RSS budget**, not just CPU count, to
//! prevent COW write-amplification OOM under fixture-heavy suites (design 05 §6.3).
//!
//! `max_concurrent_forks() = min(cpu, rss_budget / per_fork_estimate)`, where `per_fork_estimate`
//! is **seeded from `Watermark.rss_bytes`** of the fork-from layer and refined by observed child
//! RSS over the run. `admit()` reserves `per_fork_estimate` from a shared budget and hands back an
//! RAII [`ForkPermit`] that returns the bytes on drop.
//!
//! **Admission model (understand-before-applying).** Phase 3 has no concurrent fork *driver* yet —
//! `ForkWorker` runs forks sequentially and the duration-aware fan-out is the Phase 6 scheduler's
//! job. So `admit()` is a non-blocking *try-reserve*: it succeeds while the budget allows and returns
//! `EngineError::Exec` once exhausted (the caller throttles by holding ≤ `max_concurrent_forks()`
//! permits). A blocking/parking variant is deferred to Phase 6, where the scheduler actually issues
//! forks concurrently — adding it then is a pure extension, not a reshape.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::error::{EngineError, Result};
use crate::exec::fork_permit::ForkPermit;

/// RSS-budget admission controller for concurrent forks.
#[derive(Debug)]
pub struct MemoryGovernor {
    /// Total RSS budget for in-flight forks (bytes).
    rss_budget: u64,
    /// Current per-fork RSS estimate (bytes); seeded from `Watermark.rss_bytes`, refined at runtime.
    per_fork_estimate: u64,
    /// CPU-count ceiling (the other input to `min(...)`).
    cpu_limit: usize,
    /// Shared remaining budget; `admit` subtracts, the issued [`ForkPermit`] adds back on drop.
    available: Arc<AtomicU64>,
}

impl MemoryGovernor {
    /// Construct a governor with a budget, an initial per-fork estimate (the fork-from layer's
    /// `rss_bytes`), and a CPU ceiling.
    pub fn new(rss_budget: u64, per_fork_estimate: u64, cpu_limit: usize) -> Self {
        Self {
            rss_budget,
            per_fork_estimate,
            cpu_limit,
            available: Arc::new(AtomicU64::new(rss_budget)),
        }
    }

    /// The per-fork charge, never zero (a zero estimate would divide-by-zero / admit infinitely).
    fn charge(&self) -> u64 {
        self.per_fork_estimate.max(1)
    }

    /// `min(cpu, rss_budget / per_fork_estimate)`, clamped to ≥ 1 (always make progress, even when
    /// `rss_budget < per_fork_estimate`) (W13).
    pub fn max_concurrent_forks(&self) -> usize {
        let by_memory = (self.rss_budget / self.charge()).max(1);
        let by_memory = usize::try_from(by_memory).unwrap_or(usize::MAX);
        self.cpu_limit.max(1).min(by_memory)
    }

    /// Reserve `per_fork_estimate` from the budget and hand back an RAII [`ForkPermit`] (dropping it
    /// returns the bytes). `EngineError::Exec` if the budget cannot currently admit another fork
    /// (W13). Non-blocking by design — see the module docs.
    pub fn admit(&self) -> Result<ForkPermit> {
        let charge = self.charge();
        loop {
            let current = self.available.load(Ordering::SeqCst);
            if current < charge {
                return Err(EngineError::Exec(format!(
                    "memory budget exhausted: {current} B available < {charge} B per-fork estimate"
                )));
            }
            if self
                .available
                .compare_exchange(
                    current,
                    current - charge,
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                )
                .is_ok()
            {
                return Ok(ForkPermit::admitted(charge, Arc::clone(&self.available)));
            }
        }
    }

    /// Refine `per_fork_estimate` from an observed child RSS sample: take the **max** of the current
    /// estimate and the observation (conservative — overestimating costs a little parallelism;
    /// underestimating risks OOM) (W13).
    pub fn observe_rss(&mut self, observed_bytes: u64) {
        if observed_bytes > self.per_fork_estimate {
            self.per_fork_estimate = observed_bytes;
        }
    }

    /// The current per-fork RSS estimate (bytes).
    pub fn per_fork_estimate(&self) -> u64 {
        self.per_fork_estimate
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GB: u64 = 1024 * 1024 * 1024;

    #[test]
    fn max_concurrent_is_min_of_cpu_and_memory() {
        // CPU-bound: 8 GiB budget / 1 GiB est = 8, but only 4 CPUs.
        assert_eq!(MemoryGovernor::new(8 * GB, GB, 4).max_concurrent_forks(), 4);
        // Memory-bound: 2 GiB / 1 GiB = 2, under an 8-CPU ceiling.
        assert_eq!(MemoryGovernor::new(2 * GB, GB, 8).max_concurrent_forks(), 2);
    }

    #[test]
    fn max_concurrent_clamps_to_one_when_estimate_exceeds_budget() {
        // A single fork won't fit the budget — still make progress with 1.
        assert_eq!(
            MemoryGovernor::new(512 * 1024 * 1024, GB, 8).max_concurrent_forks(),
            1
        );
    }

    #[test]
    fn zero_estimate_does_not_divide_by_zero() {
        assert_eq!(MemoryGovernor::new(GB, 0, 6).max_concurrent_forks(), 6);
    }

    #[test]
    fn admit_reserves_and_denies_under_pressure_then_releases_on_drop() {
        let gov = MemoryGovernor::new(2 * GB, GB, 8); // budget admits exactly 2 in flight
        let p1 = gov.admit().expect("first admit fits");
        assert_eq!(p1.charged_bytes(), GB);
        let p2 = gov.admit().expect("second admit fits");
        // Budget now exhausted — third is denied (non-blocking try-reserve).
        assert!(
            gov.admit().is_err(),
            "no budget for a third concurrent fork"
        );
        drop(p2); // returns its GiB to the budget
        let _p3 = gov
            .admit()
            .expect("admit succeeds again after a permit drops");
        // p1 still held → only one slot freed.
        assert!(gov.admit().is_err());
        drop(p1);
    }

    #[test]
    fn observe_rss_refines_estimate_upward_and_lowers_parallelism() {
        let mut gov = MemoryGovernor::new(8 * GB, GB, 16);
        assert_eq!(gov.max_concurrent_forks(), 8);
        gov.observe_rss(2 * GB); // children are fatter than the watermark seed suggested
        assert_eq!(gov.per_fork_estimate(), 2 * GB);
        assert_eq!(
            gov.max_concurrent_forks(),
            4,
            "refined estimate halves the fan-out"
        );
        gov.observe_rss(512 * 1024 * 1024); // a smaller sample never lowers the conservative estimate
        assert_eq!(gov.per_fork_estimate(), 2 * GB);
    }
}
