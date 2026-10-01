//! The per-test execution knobs a worker runs a batch with (TID-117). They were four fields
//! redeclared on `ForkWorker`, `PooledWorker`, `run_batch`'s parameter list, the runner's
//! `BatchExec` and the plan; this is the one declaration.

use std::collections::HashSet;
use std::sync::Arc;

use crate::domain::NodeId;

/// How a batch is executed on the fork and pool tiers.
#[derive(Debug, Clone)]
pub struct RunKnobs {
    /// Per-test deadline in milliseconds.
    pub deadline_ms: u64,
    /// Take the optimistic in-process ladder for restorable tests (TID-33). The shim still forks
    /// a module it cannot snapshot-restore, so isolation is preserved.
    pub optimistic_no_fork: bool,
    /// Node ids recorded pure and unchanged: run bare no-fork, skipping the snapshot (TID-1).
    /// Honoured only under the ladder.
    pub trusted_pure: Arc<HashSet<NodeId>>,
    /// Node ids recorded as disturbing interpreter state (TID-33): never take the ladder, and
    /// get the module-child route (TID-96) — whether or not the ladder is on, which is why this
    /// is not folded into `optimistic_no_fork`.
    pub must_fork: Arc<HashSet<NodeId>>,
}

impl RunKnobs {
    /// Fork every test, with no recorded verdicts.
    pub fn new(deadline_ms: u64) -> Self {
        Self {
            deadline_ms,
            optimistic_no_fork: false,
            trusted_pure: Arc::new(HashSet::new()),
            must_fork: Arc::new(HashSet::new()),
        }
    }

    pub fn with_deadline_ms(mut self, ms: u64) -> Self {
        self.deadline_ms = ms;
        self
    }

    pub fn with_optimistic_no_fork(mut self, on: bool) -> Self {
        self.optimistic_no_fork = on;
        self
    }

    pub fn with_trusted_pure(mut self, trusted: HashSet<NodeId>) -> Self {
        self.trusted_pure = Arc::new(trusted);
        self
    }

    pub fn with_must_fork(mut self, must_fork: HashSet<NodeId>) -> Self {
        self.must_fork = Arc::new(must_fork);
        self
    }
}
