use std::collections::HashSet;
use std::path::Path;

use crate::domain::{TestItem, TestResult};
use crate::error::Result;
use crate::exec::transport::run_batch;
use crate::exec::wellspring::Wellspring;
use crate::exec::worker::Worker;

/// Default executor (Linux/macOS): one warm [`Wellspring`], fork-per-test (ADR-E003).
pub struct ForkWorker {
    wellspring: Wellspring,
    deadline_ms: u64,
    optimistic_no_fork: bool,
    trusted: HashSet<String>,
}

impl ForkWorker {
    /// Launch the worker against `root` (the directory placed on the shim's `sys.path`).
    pub fn launch(python: &str, shim: &Path, root: &Path) -> Result<Self> {
        Ok(Self {
            wellspring: Wellspring::launch(python, shim, root)?,
            deadline_ms: 5_000,
            optimistic_no_fork: false,
            trusted: HashSet::new(),
        })
    }

    /// Per-test deadline (ms) after which the forked child is killed and reported as `Error`.
    pub fn with_deadline_ms(mut self, ms: u64) -> Self {
        self.deadline_ms = ms;
        self
    }

    /// Ask the shim to run tests in-process where sound (the snapshot/restore fast path). The shim
    /// still forks any module it can't snapshot-restore, so isolation is preserved. The wellspring
    /// must have been launched with `RIPTIDE_RESTORE=1` (the daemon sets it) for this to be safe.
    pub fn with_optimistic_no_fork(mut self, on: bool) -> Self {
        self.optimistic_no_fork = on;
        self
    }

    /// Node ids known to be *pure and unchanged* (TID-1): each runs BARE no-fork (skip the snapshot).
    /// Only honored together with `with_optimistic_no_fork(true)`.
    pub fn with_trusted_pure(mut self, trusted: HashSet<String>) -> Self {
        self.trusted = trusted;
        self
    }

    /// The underlying Wellspring pid (for diagnostics/tests).
    pub fn wellspring_pid(&self) -> i64 {
        self.wellspring.pid()
    }
}

impl Worker for ForkWorker {
    fn run(&mut self, items: &[TestItem]) -> Result<Vec<TestResult>> {
        let deadline_ms = self.deadline_ms;
        let nf = self.optimistic_no_fork;
        run_batch(
            self.wellspring.transport_mut(),
            items,
            deadline_ms,
            nf,
            &self.trusted,
        )
    }
}
