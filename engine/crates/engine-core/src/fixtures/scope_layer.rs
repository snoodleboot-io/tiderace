//! `ScopeLayer` (W8) — one scope's bin of fixture instances within a [`crate::fixtures::FixturePlan`].
//!
//! The `LayeredResolver` walks a test's closure in topo order and bins each [`FixtureInstance`] into
//! the layer matching its scope. Layers are ordered Session → Package → Module → Class (Function
//! never lands in a layer — it goes to `FixturePlan.post_fork`). A layer becomes a **snapshot
//! boundary** (`snapshot = Some(..)`) when its scope is wider than Function and it is shared by ≥1
//! co-located test (design 04 §4.1–4.2). Pure data — fully defined.

use serde::{Deserialize, Serialize};

use crate::domain::{NodeId, Scope, ScopePath};
use crate::exec::WatermarkId;
use crate::fixtures::finalizer::Finalizer;
use crate::fixtures::fixture_instance::FixtureInstance;

/// One scope's worth of setup within a fixture plan.
///
/// `snapshot` is `Some(id)` once the wellspring has minted a [`crate::exec::Watermark`] for this
/// layer (a forkable point); it is a `WatermarkId` rather than the full `Watermark` so the *plan*
/// stays decoupled from live wellspring runtime state. `reinit_in_child` carries the node ids of
/// fork-fragile resources whose pure part is snapshotted at this layer but whose handle must be
/// rebuilt per child (W11, design 04 §4.3) — the encoding of split-setup at the layer level.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeLayer {
    /// The scope this layer captures (never `Function`).
    pub scope: Scope,
    /// The location this layer was resolved for.
    pub scope_path: ScopePath,
    /// The fixture instances set up at this layer, in topo (setup) order.
    pub setup: Vec<FixtureInstance>,
    /// `Some(id)` if a watermark has been minted for this layer (it is a fork source); else `None`.
    pub snapshot: Option<WatermarkId>,
    /// Fork-fragile resource fixtures (declared at this layer) to rebuild post-fork in each child.
    pub reinit_in_child: Vec<NodeId>,
    /// Teardown continuations captured at this layer, in capture order (replayed in reverse at
    /// layer retire — **once**, not per test, for snapshotted scopes).
    pub finalizers: Vec<Finalizer>,
}

impl ScopeLayer {
    /// Construct a layer for `scope` at `scope_path` with the given setup instances.
    pub fn new(scope: Scope, scope_path: ScopePath, setup: Vec<FixtureInstance>) -> Self {
        Self {
            scope,
            scope_path,
            setup,
            snapshot: None,
            reinit_in_child: Vec::new(),
            finalizers: Vec::new(),
        }
    }

    /// The scope this layer captures.
    pub fn scope(&self) -> Scope {
        self.scope
    }

    /// `true` if a watermark has been minted (this layer is a live fork source).
    pub fn is_snapshotted(&self) -> bool {
        self.snapshot.is_some()
    }

    /// The minted watermark id, if any.
    pub fn snapshot(&self) -> Option<&WatermarkId> {
        self.snapshot.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::WatermarkId;

    fn inst() -> FixtureInstance {
        use crate::fixtures::closure_hash::ClosureHash;
        FixtureInstance::new(NodeId::new("f"), None, ClosureHash::from_bytes([0u8; 32]))
    }

    #[test]
    fn new_layer_has_no_snapshot_or_reinit() {
        let l = ScopeLayer::new(Scope::Module, ScopePath::module("m.py"), vec![inst()]);
        assert_eq!(l.scope(), Scope::Module);
        assert!(!l.is_snapshotted());
        assert_eq!(l.snapshot(), None);
        assert!(l.reinit_in_child.is_empty());
        assert!(l.finalizers.is_empty());
        assert_eq!(l.setup.len(), 1);
    }

    #[test]
    fn snapshot_accessor_reflects_minted_watermark() {
        let mut l = ScopeLayer::new(Scope::Session, ScopePath::module(""), vec![]);
        l.snapshot = Some(WatermarkId::new(7));
        assert!(l.is_snapshotted());
        assert_eq!(l.snapshot(), Some(&WatermarkId::new(7)));
    }
}
