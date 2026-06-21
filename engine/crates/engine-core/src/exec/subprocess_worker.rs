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

use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use crate::domain::{TestItem, TestResult};
use crate::error::{EngineError, Result};
use crate::exec::transport::{run_batch, Live, PipeTransport, ShimTransport};
use crate::exec::worker::Worker;
use crate::exec::worker_caps::WorkerCaps;

/// No-fork fallback executor: a warm `python`+shim process, scope setup re-run (not snapshotted).
#[derive(Debug)]
pub struct SubprocessWorker {
    /// Per-test wall-clock budget (ms) before an `Outcome::Error`.
    deadline_ms: u64,
    /// Pool size (advertised parallel-test ceiling). Phase 3 runs sequentially; see module docs.
    pool_size: usize,
    /// The interpreter, shim, and corpus root to launch against (set via [`Self::with_target`]).
    target: Option<Target>,
}

#[derive(Debug, Clone)]
struct Target {
    python: String,
    shim: PathBuf,
    root: PathBuf,
}

impl SubprocessWorker {
    /// Construct a fallback worker with a deadline and pool size.
    pub fn new(deadline_ms: u64, pool_size: usize) -> Self {
        Self {
            deadline_ms,
            pool_size,
            target: None,
        }
    }

    /// Point the worker at an interpreter + shim + corpus root (the no-COW analogue of
    /// [`crate::exec::ForkWorker::launch`]'s arguments). Required before [`Worker::run`].
    pub fn with_target(mut self, python: impl Into<String>, shim: &Path, root: &Path) -> Self {
        self.target = Some(Target {
            python: python.into(),
            shim: shim.to_path_buf(),
            root: root.to_path_buf(),
        });
        self
    }

    /// Advertise no-COW capabilities so the scheduler prefers larger batches / pure-LPT balancing.
    pub fn capabilities(&self) -> WorkerCaps {
        WorkerCaps::subprocess(self.pool_size)
    }

    /// Launch the no-fork wellspring (`python <shim> <root> --no-fork`) and complete the handshake.
    fn launch(target: &Target) -> Result<NoForkProc> {
        let mut child = Command::new(&target.python)
            .arg(&target.shim)
            .arg(&target.root)
            .arg("--no-fork")
            // Pin native thread pools (threaded BLAS/OMP is a hazard even without fork).
            .env("OPENBLAS_NUM_THREADS", "1")
            .env("OMP_NUM_THREADS", "1")
            .env("MKL_NUM_THREADS", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|e| EngineError::Exec(format!("failed to launch subprocess worker: {e}")))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| EngineError::Exec("subprocess worker stdin unavailable".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| EngineError::Exec("subprocess worker stdout unavailable".into()))?;

        let mut transport = PipeTransport::new(stdin, BufReader::new(stdout));
        transport.ready()?;
        Ok(NoForkProc { child, transport })
    }
}

impl Worker for SubprocessWorker {
    fn run(&mut self, items: &[TestItem]) -> Result<Vec<TestResult>> {
        let target = self.target.clone().ok_or_else(|| {
            EngineError::Exec("SubprocessWorker has no target; call with_target".into())
        })?;
        let mut proc = SubprocessWorker::launch(&target)?;
        run_batch(&mut proc.transport, items, self.deadline_ms)
    }
}

/// A live no-fork wellspring process + its framed pipe (mirrors `Wellspring`, minus the fork).
struct NoForkProc {
    child: Child,
    transport: Live,
}

impl Drop for NoForkProc {
    fn drop(&mut self) {
        self.transport.close_input(); // EOF → shim exits + runs wider-scope finalizers once
        let _ = self.child.wait();
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
