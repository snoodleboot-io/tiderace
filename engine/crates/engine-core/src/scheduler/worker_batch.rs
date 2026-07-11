use crate::domain::NodeId;

/// The ordered work assigned to one fork worker (design 06 §2). Carries the test node ids in run
/// order plus the batch's estimated total duration (the bin load used for makespan balancing). A
/// batch is built to maximize snapshot reuse — tests sharing a locality key land in the same batch
/// unless a too-large group was deliberately split.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerBatch {
    worker: usize,
    items: Vec<NodeId>,
    est_total_ms: u64,
}

impl WorkerBatch {
    pub fn new(worker: usize) -> Self {
        Self {
            worker,
            items: Vec::new(),
            est_total_ms: 0,
        }
    }

    /// Append a test and add its estimated duration to the bin load.
    pub fn push(&mut self, node_id: NodeId, duration_ms: u64) {
        self.items.push(node_id);
        self.est_total_ms += duration_ms;
    }

    pub fn worker(&self) -> usize {
        self.worker
    }

    pub fn items(&self) -> &[NodeId] {
        &self.items
    }

    /// The estimated total duration of this batch (its bin load).
    pub fn est_total_ms(&self) -> u64 {
        self.est_total_ms
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}
