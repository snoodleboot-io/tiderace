//! `SubprocessWorker` (W12) — the no-COW fallback `Worker` for Windows / `--no-fork` / fork-unsafe
//! stacks (design 05 §7, ADR-E008).
//!
//! With no COW it cannot inherit snapshot state, so it takes the no-COW path: re-run the batch's
//! **wider-than-Function** scope setup **once per worker**, run each test's Function setup/body/
//! teardown sequentially, then run wider-scope finalizers **once** at batch end. It is required to
//! be **result-identical** to the fork path (the contract invariant verified at §8 boundary 3).
//!
//! **Contract seam.** Struct shape + `Worker` impl + `capabilities` signature frozen here; the
//! no-COW execution logic is implemented by Lane FALLBACK (subagent fb-subproc).

use crate::domain::{TestItem, TestResult};
use crate::error::Result;
use crate::exec::worker::Worker;
use crate::exec::worker_caps::WorkerCaps;

/// No-fork fallback executor: a warm `python`+shim process pool, scope setup re-run per worker.
// `deadline_ms` is written by `new` and read by the no-COW execution loop Lane FALLBACK
// (fb-subproc) fills in; allowed dead until that scaffold is replaced (W12).
#[allow(dead_code)]
#[derive(Debug)]
pub struct SubprocessWorker {
    /// Per-test wall-clock budget (ms) before `kill_tree` and an `Outcome::Error`.
    deadline_ms: u64,
    /// Pool size (warm worker processes); also the parallel-test ceiling advertised in caps.
    pool_size: usize,
}

impl SubprocessWorker {
    /// Construct a fallback worker with a deadline and pool size.
    ///
    /// Defined (trivial field init) so lanes/tests can construct one without a scaffold.
    pub fn new(deadline_ms: u64, pool_size: usize) -> Self {
        Self {
            deadline_ms,
            pool_size,
        }
    }

    /// Advertise no-COW capabilities so the scheduler prefers larger batches / pure-LPT balancing.
    ///
    /// Defined (pure derivation) — `supports_cow == false` is the load-bearing fact and is not a
    /// lane decision.
    pub fn capabilities(&self) -> WorkerCaps {
        WorkerCaps::subprocess(self.pool_size)
    }
}

impl Worker for SubprocessWorker {
    fn run(&mut self, _items: &[TestItem]) -> Result<Vec<TestResult>> {
        unimplemented!(
            "LANE: Lane FALLBACK (fb-subproc) implements SubprocessWorker::run — W12 (no-COW scope re-run, result-identical to fork)"
        )
    }
}
