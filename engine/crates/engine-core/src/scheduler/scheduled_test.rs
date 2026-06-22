use crate::domain::NodeId;

/// One test handed to the [`Scheduler`](crate::scheduler::Scheduler), annotated with the two things
/// scheduling reconciles (ADR-E010): its **locality key** — the deepest snapshot scope it shares with
/// other tests (`session` / `module:<path>` / `class:<path>::<C>`), so co-located tests reuse one
/// per-worker snapshot — and its **estimated duration** (from timing history / cache; a heuristic on a
/// cold run) for longest-processing-time makespan balancing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduledTest {
    node_id: NodeId,
    locality_key: String,
    duration_ms: u64,
}

impl ScheduledTest {
    pub fn new(node_id: NodeId, locality_key: impl Into<String>, duration_ms: u64) -> Self {
        Self {
            node_id,
            locality_key: locality_key.into(),
            duration_ms,
        }
    }

    pub fn node_id(&self) -> &NodeId {
        &self.node_id
    }

    /// The deepest shared snapshot scope (the locality grouping key).
    pub fn locality_key(&self) -> &str {
        &self.locality_key
    }

    pub fn duration_ms(&self) -> u64 {
        self.duration_ms
    }
}
