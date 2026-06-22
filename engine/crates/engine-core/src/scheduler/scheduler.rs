use crate::scheduler::{ScheduledTest, WorkerBatch};

/// Input to a scheduling pass: the surviving tests (after cache/impact filtering) and the worker count.
#[derive(Debug, Clone)]
pub struct ScheduleInput {
    tests: Vec<ScheduledTest>,
    workers: usize,
}

impl ScheduleInput {
    /// `workers` is clamped to at least 1 (a degenerate 0 would yield no batches).
    pub fn new(tests: Vec<ScheduledTest>, workers: usize) -> Self {
        Self {
            tests,
            workers: workers.max(1),
        }
    }

    pub fn tests(&self) -> &[ScheduledTest] {
        &self.tests
    }

    pub fn workers(&self) -> usize {
        self.workers
    }
}

/// The scheduling seam (ADR-E005/E010): decide which worker runs which tests, in which order. Runs as
/// cheap Rust after cache/impact filtering, before fork. The production impl is
/// [`LocalityScheduler`](crate::scheduler::LocalityScheduler); a locality-blind
/// [`RoundRobinScheduler`](crate::scheduler::RoundRobinScheduler) is kept for debugging + as the
/// makespan baseline.
pub trait Scheduler {
    /// Produce one [`WorkerBatch`] per worker (empty batches omitted).
    fn plan(&self, input: &ScheduleInput) -> Vec<WorkerBatch>;
}

/// The makespan of a plan: the maximum bin load — what wall-clock the slowest worker dictates.
pub fn makespan(batches: &[WorkerBatch]) -> u64 {
    batches
        .iter()
        .map(WorkerBatch::est_total_ms)
        .max()
        .unwrap_or(0)
}
