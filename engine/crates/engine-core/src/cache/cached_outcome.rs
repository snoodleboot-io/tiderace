use serde::{Deserialize, Serialize};

use crate::domain::Outcome;

/// The stored artifact for one content-addressed test result (ADR-E004): the outcome plus its
/// captured diagnostics, enough to reconstruct a [`crate::domain::TestResult`] on a cache hit without
/// re-running the test. (`duration_ms` is intentionally **not** part of the key's identity — it is
/// recorded for reporting but a hit reports it as cached/zero.)
///
/// `Serialize`/`Deserialize` so a shareable tier ([`DirCache`](crate::cache::DirCache)) can persist it
/// across machines/CI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedOutcome {
    outcome: Outcome,
    detail: String,
}

impl CachedOutcome {
    pub fn new(outcome: Outcome, detail: impl Into<String>) -> Self {
        Self {
            outcome,
            detail: detail.into(),
        }
    }

    pub fn outcome(&self) -> Outcome {
        self.outcome
    }

    pub fn detail(&self) -> &str {
        &self.detail
    }
}
