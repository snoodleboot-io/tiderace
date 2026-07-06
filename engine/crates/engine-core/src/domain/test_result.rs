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
    /// Source files this test touched (relative paths), from coverage — the test's dependency
    /// footprint, used by impact-aware re-runs. Empty unless coverage capture was on.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub touched_files: Vec<String>,
    /// Purity verdict (TID-1): `Some(true)` measured pure, `Some(false)` impure, `None` not measured.
    /// A recorded `Some(true)` promotes an unchanged test to the bare-no-fork tier on the next run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pure: Option<bool>,
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
            touched_files: Vec::new(),
            pure: None,
        }
    }

    /// Attach the touched-file footprint (builder style).
    pub fn with_touched(mut self, touched_files: Vec<String>) -> Self {
        self.touched_files = touched_files;
        self
    }

    /// Attach the purity verdict (builder style).
    pub fn with_pure(mut self, pure: Option<bool>) -> Self {
        self.pure = pure;
        self
    }
}
