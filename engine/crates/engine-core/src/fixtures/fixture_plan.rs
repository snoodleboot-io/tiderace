//! `FixturePlan` (W8) — the deliverable the executor + scheduler consume (design 04 §4, §6).
//!
//! Produced by the `LayeredResolver` for a given test: its closure partitioned into scope layers,
//! the deepest shared snapshot to fork from, the Function-scope setup to run post-fork, the assembled
//! argument map, and the cache-key `closure_hash`. Pure data — fully defined. (Construction is Lane
//! FX-graph's job; the *shape* is frozen here.)

use serde::{Deserialize, Serialize};

use crate::domain::NodeId;
use crate::exec::WatermarkId;
use crate::fixtures::closure_hash::ClosureHash;
use crate::fixtures::fixture_args::FixtureArgs;
use crate::fixtures::fixture_instance::FixtureInstance;
use crate::fixtures::scope_layer::ScopeLayer;

/// The fully-resolved fixture plan for one test.
///
/// `fork_from` is `Some(deepest_shared)` — the narrowest-scoped **live** snapshot shared by this
/// test (`WatermarkStack::deepest_shared`); `None` means fork from the wellspring base (Layer 1, no
/// wider-scope fixtures apply). `post_fork` is the Function-scope setup (plus `reinit_after_fork`
/// resources) run **in the forked child**. Phase 4 consumes `post_fork` + `fixture_args`; Phase 5
/// consumes `closure_hash`; Phase 6 sizes fan-out from the layers' watermark `rss_bytes`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FixturePlan {
    /// The test this plan is for.
    pub test: NodeId,
    /// Scope layers ordered widest → narrowest (Session → Package → Module → Class); never Function.
    pub layers: Vec<ScopeLayer>,
    /// The deepest shared live snapshot to fork from, or `None` to fork from the wellspring base.
    pub fork_from: Option<WatermarkId>,
    /// Function-scope fixture instances set up in the forked child, in topo order.
    pub post_fork: Vec<FixtureInstance>,
    /// The assembled argument map the body is invoked with.
    pub fixture_args: FixtureArgs,
    /// The cache-key term over the resolved closure (W14, ADR-E004).
    pub closure_hash: ClosureHash,
}

impl FixturePlan {
    /// Assemble a plan from its parts.
    pub fn new(
        test: NodeId,
        layers: Vec<ScopeLayer>,
        fork_from: Option<WatermarkId>,
        post_fork: Vec<FixtureInstance>,
        fixture_args: FixtureArgs,
        closure_hash: ClosureHash,
    ) -> Self {
        Self {
            test,
            layers,
            fork_from,
            post_fork,
            fixture_args,
            closure_hash,
        }
    }

    /// The test this plan is for.
    pub fn test(&self) -> &NodeId {
        &self.test
    }

    /// The snapshot to fork from, if any.
    pub fn fork_from(&self) -> Option<&WatermarkId> {
        self.fork_from.as_ref()
    }

    /// The plan's cache-key closure hash.
    pub fn closure_hash(&self) -> ClosureHash {
        self.closure_hash
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Scope;
    use crate::exec::WatermarkId;
    use crate::fixtures::scope_layer::ScopeLayer;

    fn hash() -> ClosureHash {
        ClosureHash::from_bytes([3u8; 32])
    }

    #[test]
    fn accessors_round_trip() {
        let layers = vec![ScopeLayer::new(
            Scope::Session,
            crate::domain::ScopePath::module(""),
            vec![],
        )];
        let plan = FixturePlan::new(
            NodeId::new("test_x.py::t"),
            layers,
            Some(WatermarkId::new(9)),
            Vec::new(),
            FixtureArgs::new(),
            hash(),
        );
        assert_eq!(plan.test(), &NodeId::new("test_x.py::t"));
        assert_eq!(plan.fork_from(), Some(&WatermarkId::new(9)));
        assert_eq!(plan.closure_hash(), hash());
        assert_eq!(plan.layers.len(), 1);
    }

    #[test]
    fn fork_from_none_means_wellspring_base() {
        let plan = FixturePlan::new(
            NodeId::new("m.py::t"),
            Vec::new(),
            None,
            Vec::new(),
            FixtureArgs::new(),
            hash(),
        );
        assert_eq!(plan.fork_from(), None);
    }
}
