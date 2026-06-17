use crate::domain::{TestItem, TestResult};
use crate::error::Result;

/// Executes tests and returns one [`TestResult`] per item. The DIP seam ([ADR-E005]) behind which
/// `ForkWorker` (default), `SubprocessWorker` (no-fork fallback, ADR-E008), `ThreadWorker`
/// (free-threaded), and `RemoteWorker` (distributed) live, so the orchestrator never speaks `fork`.
pub trait Worker {
    fn run(&mut self, items: &[TestItem]) -> Result<Vec<TestResult>>;
}
