//! `FixtureInstance` (W5) — a fixture definition bound to a concrete parameter selection.
//!
//! A parametrized fixture with `params = [a, b, c]` fans out into three `FixtureInstance`s, each
//! with its **own** `closure_hash` so the variants cache independently (design 04 §1.3, §8). An
//! unparametrized fixture produces exactly one instance with `param = None`. Pure data — fully
//! defined.

use serde::{Deserialize, Serialize};

use crate::domain::NodeId;
use crate::fixtures::closure_hash::ClosureHash;
use crate::fixtures::param_value::ParamValue;

/// A fixture bound to a concrete parameter set, carrying its own closure hash.
///
/// This is the unit that is set up (in the wellspring lineage for snapshotted scopes, or in the
/// forked child for `post_fork`/Function scope) and the unit a `Finalizer` tears down.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FixtureInstance {
    /// The fixture definition this instance realizes.
    pub fixture: NodeId,
    /// The selected parameter, or `None` for an unparametrized fixture.
    pub param: Option<ParamValue>,
    /// This instance's cache identity — distinct per parametrization variant (W14).
    pub closure_hash: ClosureHash,
}

impl FixtureInstance {
    /// Construct an instance for `fixture` with an (optional) selected parameter and its closure hash.
    pub fn new(fixture: NodeId, param: Option<ParamValue>, closure_hash: ClosureHash) -> Self {
        Self {
            fixture,
            param,
            closure_hash,
        }
    }

    /// The fixture definition node id.
    pub fn fixture(&self) -> &NodeId {
        &self.fixture
    }

    /// The selected parameter, if this instance is a parametrization variant.
    pub fn param(&self) -> Option<&ParamValue> {
        self.param.as_ref()
    }

    /// This instance's closure hash (its cache identity).
    pub fn closure_hash(&self) -> ClosureHash {
        self.closure_hash
    }
}
