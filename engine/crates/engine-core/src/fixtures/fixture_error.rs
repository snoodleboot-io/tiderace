//! The fixture error taxonomy — a **public contract surface** (ratified Phase 3, C-FX).
//!
//! These variants are consumed and reported on by Phases 4/5/7, so their *shape* is frozen at the
//! Phase 3 contract step. They are distinct from [`crate::error::EngineError`]: a `FixtureError` is
//! a fixture-graph *validity* failure surfaced during collection/resolution (a user error in the
//! fixture topology), not an infrastructure failure and not a test `Outcome::Error`.

use thiserror::Error;

use crate::domain::{Scope, ScopePath};

/// A fixture-graph validity error, raised at build/resolve time (never a panic — per rust.md).
///
/// Frozen taxonomy (Phase 3 CONTRACT.md). New variants may be **added** by later phases without
/// breaking consumers, but existing variants and their fields do not change shape.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum FixtureError {
    /// A dependency cycle in the fixture DAG (e.g. `a → b → a`). The run aborts collection for the
    /// offending scope path rather than deadlocking at setup. `path` is the cycle, in request order,
    /// by fixture name (first element repeats as the last conceptually — emit the names along the
    /// back-edge).
    #[error("fixture dependency cycle: {}", .path.join(" -> "))]
    Cycle {
        /// Fixture names along the cycle, in request order.
        path: Vec<String>,
    },

    /// Scope-monotonicity violation: a wider-scoped fixture depends on a narrower-scoped one, which
    /// would force the wider scope to be re-set-up per child and defeat snapshotting. The offender
    /// is the *wider* fixture depending downward; `narrow` is the (illegal) dependency's scope,
    /// `wide` is the depending fixture's own (wider) scope. Invariant required: `wide.outlives(narrow)`.
    #[error(
        "scope widening: a {wide:?}-scoped fixture depends on a narrower {narrow:?}-scoped fixture"
    )]
    ScopeWiden {
        /// The narrower scope of the depended-upon fixture (the illegal target).
        narrow: Scope,
        /// The wider scope of the depending fixture (the offender).
        wide: Scope,
    },

    /// A requested (or transitively required) fixture name could not be resolved to any definition
    /// visible from the given scope path (no override entry is a prefix of the request location).
    #[error("unresolved fixture '{name}' for scope path {scope_path:?}")]
    Unresolved {
        /// The unresolved fixture name.
        name: String,
        /// The location from which resolution was attempted.
        scope_path: ScopePath,
    },

    /// Two `autouse` fixtures collide at the same effective location (ambiguous injection). The
    /// `name` is the minimal identifier of the offending duplicate.
    #[error("duplicate autouse fixture '{name}'")]
    DuplicateAutouse {
        /// The duplicated autouse fixture name.
        name: String,
    },

    /// A parametrized fixture's declared parameter shape is inconsistent (e.g. an `ids` list whose
    /// length does not match `params`, or a dependent's expectation of the param arity is violated).
    #[error("parameter shape mismatch for fixture '{name}'")]
    ParamShapeMismatch {
        /// The offending fixture name.
        name: String,
    },
}
