use crate::scheduler::{ScheduleInput, Scheduler, WorkerBatch};

/// The locality-blind baseline (today's `tiderace/runner.rs` behavior): deal tests to workers
/// cyclically, ignoring both duration and snapshot scope. Kept for debugging and as the makespan
/// baseline the [`LocalityScheduler`](crate::scheduler::LocalityScheduler) must beat (ADR-E010).
#[derive(Debug, Clone, Copy, Default)]
pub struct RoundRobinScheduler;

impl Scheduler for RoundRobinScheduler {
    fn plan(&self, input: &ScheduleInput) -> Vec<WorkerBatch> {
        let n = input.workers();
        let mut batches: Vec<WorkerBatch> = (0..n).map(WorkerBatch::new).collect();
        for (i, test) in input.tests().iter().enumerate() {
            batches[i % n].push(test.node_id().clone(), test.duration_ms());
        }
        batches.retain(|b| !b.is_empty());
        batches
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::NodeId;
    use crate::scheduler::ScheduledTest;

    #[test]
    fn deals_tests_cyclically() {
        let input = ScheduleInput::new(
            (0..5)
                .map(|i| ScheduledTest::new(NodeId::new(format!("t{i}")), "m", 1))
                .collect(),
            2,
        );
        let batches = RoundRobinScheduler.plan(&input);
        assert_eq!(batches.len(), 2);
        assert_eq!(batches[0].items().len(), 3); // t0,t2,t4
        assert_eq!(batches[1].items().len(), 2); // t1,t3
    }
}
