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
    /// This test disturbed interpreter state nothing undid, so later runs must fork it from the
    /// start rather than rediscover the problem (TID-33).
    ///
    /// Distinct from `pure == Some(false)`: most impure tests are impure in ways restore handles
    /// completely, and forking all of them would cost the in-process ladder nearly everything it
    /// buys. This marks only the ones whose damage survived the restore.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub must_fork: bool,
    /// The module whose import skipped, when this test was skipped because a *whole module* did not
    /// import (TID-55) — a `pytest.importorskip` in the module or in a conftest above it. Empty for
    /// a per-test skip and for every non-skip outcome.
    ///
    /// One skip event can produce hundreds of skipped tests. Both numbers are worth reporting and
    /// neither substitutes for the other, so the origin is kept rather than the count.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub skip_origin: String,
    /// This node id came from *runtime expansion* — a parametrize case or an inherited method the
    /// collector could not see statically — rather than from static collection (TID-55).
    ///
    /// Useful to anyone diffing our node ids against another runner's: an id that exists on one side
    /// only is a different kind of disagreement than an id whose outcome changed, and expansion is
    /// where those extra ids come from.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub expanded: bool,
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
            must_fork: false,
            skip_origin: String::new(),
            expanded: false,
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

    /// Mark the test as one that must be forked on later runs (builder style).
    pub fn with_must_fork(mut self, must_fork: bool) -> Self {
        self.must_fork = must_fork;
        self
    }

    /// Name the module whose import skipped this test (builder style).
    pub fn with_skip_origin(mut self, skip_origin: impl Into<String>) -> Self {
        self.skip_origin = skip_origin.into();
        self
    }

    /// Mark this node as produced by runtime expansion rather than static collection (builder style).
    pub fn with_expanded(mut self, expanded: bool) -> Self {
        self.expanded = expanded;
        self
    }
}
