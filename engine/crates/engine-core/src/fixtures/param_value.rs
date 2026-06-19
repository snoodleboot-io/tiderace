//! The serializable representation of one parametrized-fixture parameter.
//!
//! **Design rationale (param value repr).** A parametrized fixture's actual values are arbitrary
//! Python objects that cannot — and must not — cross the Rust↔shim boundary as live values. pytest
//! itself never reasons over the *value*; it reasons over the **param id** (the stringified
//! identifier that appears in the node id, e.g. `test_x[a-b]`) and the param **index**. The Rust
//! engine needs exactly two things from a parameter: (1) a stable, hashable identity so each
//! [`crate::fixtures::FixtureInstance`] gets a distinct `closure_hash` (W5/W14), and (2) the index
//! the shim uses to select the live value in-child. `ParamValue` carries precisely those — nothing
//! that would require materializing a Python object in Rust.

use serde::{Deserialize, Serialize};

/// One parameter of a parametrized fixture: a stable id plus its declaration index.
///
/// Fully defined pure data (no scaffold). `Hash`/`Eq` make it usable directly in `closure_hash`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ParamValue {
    /// The param id — pytest's stringified identifier (the bracketed token in a node id). Stable
    /// across runs for a given declaration; the unit of cache-key identity for the variant.
    pub id: String,
    /// The declaration index of this parameter within the fixture's `params` list. The shim uses it
    /// to select the live value in-child.
    pub index: usize,
}

impl ParamValue {
    /// Construct a parameter from its id and declaration index.
    pub fn new(id: impl Into<String>, index: usize) -> Self {
        Self {
            id: id.into(),
            index,
        }
    }

    /// The param id (the bracketed token in the node id).
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The declaration index within the fixture's `params` list.
    pub fn index(&self) -> usize {
        self.index
    }
}
