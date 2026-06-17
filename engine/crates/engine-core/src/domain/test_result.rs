use serde::{Deserialize, Serialize};

use super::{NodeId, Outcome};

/// The result of executing one test.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestResult {
    pub node_id: NodeId,
    pub outcome: Outcome,
    pub duration_ms: u64,
    /// Failure/error detail (traceback or message); empty on success.
    pub detail: String,
}

impl TestResult {
    pub fn new(
        node_id: NodeId,
        outcome: Outcome,
        duration_ms: u64,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            node_id,
            outcome,
            duration_ms,
            detail: detail.into(),
        }
    }
}
