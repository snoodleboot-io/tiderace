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

    /// The work **units** to hand out, heaviest first — a queue the runner drains rather than a
    /// partition it commits to up front (TID-52).
    ///
    /// A static partition has to predict each worker's total cost before anything has run, and on a
    /// cold run the only weight it has is one-per-test. pirn-agents' per-test cost spans four orders
    /// of magnitude, so bins balanced by test count came out at 121/97/66/34/31/25/23/19 seconds:
    /// 57% of the machine idle, and 2.32x the makespan a perfectly balanced run would take. A queue
    /// needs no prediction — a worker that finishes early takes the next unit.
    ///
    /// The default is the static plan, one unit per worker, which is what a scheduler that *is* a
    /// partition (round-robin, the makespan baseline) should keep doing.
    fn units(&self, input: &ScheduleInput) -> Vec<WorkerBatch> {
        self.plan(input)
    }
}

/// The makespan of a plan: the maximum bin load — what wall-clock the slowest worker dictates.
pub fn makespan(batches: &[WorkerBatch]) -> u64 {
    batches
        .iter()
        .map(WorkerBatch::est_total_ms)
        .max()
        .unwrap_or(0)
}
