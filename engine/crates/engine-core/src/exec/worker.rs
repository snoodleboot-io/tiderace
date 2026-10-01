use crate::domain::{TestItem, TestResult};
use crate::error::Result;

/// Executes tests and returns one [`TestResult`] per item. The DIP seam ([ADR-E005]) behind which
/// `ForkWorker` (default), `SubprocessWorker` (no-fork fallback, ADR-E008), `ThreadWorker`
/// (free-threaded), and `RemoteWorker` (distributed) live, so the orchestrator never speaks `fork`.
pub trait Worker {
    fn run(&mut self, items: &[TestItem]) -> Result<Vec<TestResult>>;

    /// Whether this worker stopped answering during its last batch and is gone (TID-93): the
    /// batch's results were still reported, and the owner must not hand it another.
    fn is_lost(&self) -> bool {
        false
    }

    /// The worker process's pid, when it is a process of its own: what its memory is read from
    /// (TID-106). `None` for a tier that runs in the engine's own process.
    fn pid(&self) -> Option<u32> {
        None
    }
}
