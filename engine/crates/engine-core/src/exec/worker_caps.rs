//! `WorkerCaps` — capability descriptor a [`crate::exec::Worker`] advertises so the scheduler can
//! adapt (e.g. drop snapshot-locality grouping to pure LPT when `supports_cow == false`) (design 05
//! §4). Pure data — fully defined.

/// What a worker implementation can do. The scheduler reads this to size and shape batches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkerCaps {
    /// `true` for `ForkWorker` (true COW fork from a snapshot); `false` for `SubprocessWorker`
    /// (re-runs scope setup per worker). When `false` the scheduler prefers larger batches.
    pub supports_cow: bool,
    /// `true` if the worker streams `ExecEvent`s (live output/coverage) rather than a single reply.
    pub supports_streaming: bool,
    /// Maximum tests the worker can run in parallel (CPU/process bound, before RSS governance).
    pub max_parallel: usize,
}

impl WorkerCaps {
    /// Capabilities of the primary fork path (COW + streaming).
    pub fn fork(max_parallel: usize) -> Self {
        Self {
            supports_cow: true,
            supports_streaming: true,
            max_parallel,
        }
    }

    /// Capabilities of the no-fork fallback (no COW).
    pub fn subprocess(max_parallel: usize) -> Self {
        Self {
            supports_cow: false,
            supports_streaming: true,
            max_parallel,
        }
    }
}
