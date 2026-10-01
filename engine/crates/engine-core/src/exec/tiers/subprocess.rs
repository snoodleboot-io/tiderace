//! `SubprocessWorker` (W12) — the no-COW fallback `Worker` for Windows / `--no-fork` / fork-unsafe
//! stacks (design 05 §7, ADR-E008).
//!
//! With no COW it cannot inherit snapshot state, so it takes the no-COW path: a warm `python`+shim
//! process runs the batch's **wider-than-Function** scope setup **once** (in-process, not snapshotted),
//! runs each test's Function setup/body/teardown **in that same process** (no fork), and runs the
//! wider-scope finalizers **once** at batch end. Because it drives the *same* fixture engine as the
//! fork path (the shim's `--no-fork` mode), it is **result-identical** to `ForkWorker` — the contract
//! invariant verified at §8 boundary 3.
//!
//! Phase 3 executes the batch sequentially in a single no-fork wellspring (the no-COW path's
//! *correctness* is the deliverable; `pool_size`-way partitioning for throughput is a Phase 6
//! scheduling concern — adding it is a pure extension).

use std::path::{Path, PathBuf};
use std::process::ChildStdin;
use std::time::Duration;

use crate::domain::{TestItem, TestResult};
use crate::error::{EngineError, Result};
use crate::exec::process::{ShimLaunch, ShimMode, ShimProcess, ShimTarget};
use crate::exec::transport::{
    run_batch_lost, BudgetedTransport, LostWorker, ShimTransport, LOST_WORKER_MARGIN_MS,
};
use crate::exec::worker::Worker;
use crate::exec::worker_caps::WorkerCaps;

/// No-fork fallback executor: a warm `python`+shim process, scope setup re-run (not snapshotted).
pub struct SubprocessWorker {
    /// Per-test wall-clock budget (ms) before an `Outcome::Error`.
    deadline_ms: u64,
    /// Pool size (advertised parallel-test ceiling). Phase 3 runs sequentially; see module docs.
    pool_size: usize,
    /// The interpreter, shim, and corpus root to launch against (set via [`Self::with_target`]).
    target: Option<Target>,
    /// The live process, launched on first use and **kept** for this worker's lifetime (TID-52).
    ///
    /// It used to be launched and torn down inside every `run`, which was invisible while a worker
    /// ran exactly one batch. Under the work queue a worker runs many units, and relaunching per
    /// unit would mean one interpreter per module — on pirn-core, 529 process launches in place of
    /// 8. Keeping it also makes this tier's warmth match the fork tier's, where the wellspring has
    /// always outlived the batch.
    proc: Option<NoForkProc>,
}

impl std::fmt::Debug for SubprocessWorker {
    /// Hand-written because the live process holds pipe handles that are not `Debug`; the fields
    /// worth printing are the configuration, plus whether a process is up.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubprocessWorker")
            .field("deadline_ms", &self.deadline_ms)
            .field("pool_size", &self.pool_size)
            .field("target", &self.target)
            .field("launched", &self.proc.is_some())
            .finish()
    }
}

#[derive(Debug, Clone)]
struct Target {
    shim: ShimTarget,
    /// A file naming the modules this run executes, for a selective start-up (TID-75).
    modules: Option<PathBuf>,
}

impl SubprocessWorker {
    /// Construct a fallback worker with a deadline and pool size.
    pub fn new(deadline_ms: u64, pool_size: usize) -> Self {
        Self {
            deadline_ms,
            pool_size,
            target: None,
            proc: None,
        }
    }

    /// Point the worker at an interpreter + shim + corpus root (the no-COW analogue of
    /// [`crate::exec::ForkWorker::launch`]'s arguments). Required before [`Worker::run`].
    pub fn with_target(mut self, python: impl Into<String>, shim: &Path, root: &Path) -> Self {
        self.target = Some(Target {
            shim: ShimTarget::new(python, shim, root),
            modules: None,
        });
        self
    }

    /// [`with_target`](Self::with_target) from a [`ShimTarget`].
    pub fn with_shim_target(mut self, shim: ShimTarget) -> Self {
        self.target = Some(Target {
            shim,
            modules: None,
        });
        self
    }

    /// Import only the modules named in `file` at start-up (TID-75). Requires [`with_target`].
    pub fn with_modules(mut self, file: &Path) -> Self {
        if let Some(t) = self.target.as_mut() {
            t.modules = Some(file.to_path_buf());
        }
        self
    }

    /// Advertise no-COW capabilities so the scheduler prefers larger batches / pure-LPT balancing.
    pub fn capabilities(&self) -> WorkerCaps {
        WorkerCaps::subprocess(self.pool_size)
    }

    /// Launch the no-fork wellspring (`python <shim> <root> --no-fork --restore`) and complete
    /// the handshake. Restore is always on: without fork there is no COW copy, so the snapshot
    /// is this tier's only isolation, and it must not depend on the caller's environment.
    fn launch(target: &Target, deadline_ms: u64) -> Result<NoForkProc> {
        let mut process = ShimLaunch::new(&target.shim, ShimMode::NoFork)
            .modules(target.modules.as_deref())
            .spawn()?;
        let (stdin, stdout) = process.take_pipes()?;
        // The engine's own deadline on every reply (TID-98): the shim's in-process timeout ends
        // what CPython can interrupt; a test blocked in a C call is ended here, by the budget —
        // the only per-test deadline Windows has, where the shim cannot arm a timer signal.
        let budget = Duration::from_millis(deadline_ms.saturating_add(LOST_WORKER_MARGIN_MS));
        let mut transport = BudgetedTransport::new(stdin, stdout, budget);
        transport.ready()?;
        Ok(NoForkProc { transport, process })
    }
}

impl Worker for SubprocessWorker {
    fn run(&mut self, items: &[TestItem]) -> Result<Vec<TestResult>> {
        if self.proc.is_none() {
            let target = self.target.clone().ok_or_else(|| {
                EngineError::Exec("SubprocessWorker has no target; call with_target".into())
            })?;
            self.proc = Some(SubprocessWorker::launch(&target, self.deadline_ms)?);
        }
        let proc = self.proc.as_mut().expect("just launched");
        let (results, fault) = run_batch_lost(
            &mut proc.transport,
            items,
            self.deadline_ms,
            false,
            &std::collections::HashSet::new(),
            // No ladder to gate: `force_no_fork` is already false, and this tier runs in-process by
            // configuration rather than by optimistic guess (TID-33).
            &std::collections::HashSet::new(),
            // A worker that stops answering is reported per node — the one that overran names
            // the fault, the rest of the batch names it as not run — and replaced (TID-98).
            LostWorker::Report,
        )?;
        if fault.is_some() {
            self.proc = None; // dropped: killed if lost, reaped either way
        }
        Ok(results)
    }

    fn pid(&self) -> Option<u32> {
        self.proc.as_ref().map(|p| p.process.pid())
    }
}

/// A live no-fork wellspring process + its framed pipe. Fields drop in order: the transport
/// first (EOF → the shim runs its wider-scope finalizers once and exits), then the process,
/// which kills it if it was lost — still inside the test that overran — and reaps it.
struct NoForkProc {
    transport: BudgetedTransport<ChildStdin>,
    process: ShimProcess,
}

impl Drop for NoForkProc {
    fn drop(&mut self) {
        if self.transport.is_lost() {
            self.process.mark_lost();
        }
        self.transport.close_input();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_report_no_cow() {
        let caps = SubprocessWorker::new(5_000, 4).capabilities();
        assert!(!caps.supports_cow, "the fallback path has no COW");
        assert_eq!(caps.max_parallel, 4);
    }

    #[test]
    fn run_without_target_is_an_error_not_a_panic() {
        let mut w = SubprocessWorker::new(5_000, 1);
        assert!(
            w.run(&[]).is_err(),
            "no target → typed error, never a panic"
        );
    }
}
