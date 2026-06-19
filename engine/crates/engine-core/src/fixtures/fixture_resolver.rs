//! `FixtureResolver` — the DIP seam (design 04 §3, `<<trait>>`) that turns a test's closure into a
//! [`FixturePlan`]. A **trait** (not a concrete type) because it is the abstraction the scheduler/
//! executor depend on; [`crate::fixtures::LayeredResolver`] is the Phase 3 implementation and future
//! resolvers (e.g. a cache-aware one) slot in behind it without touching consumers.
//!
//! **Contract seam.** Pure signatures — no scaffold needed (trait methods have no bodies).

use crate::domain::ScopePath;
use crate::fixtures::fixture_closure::FixtureClosure;
use crate::fixtures::fixture_error::FixtureError;
use crate::fixtures::fixture_graph::FixtureGraph;
use crate::fixtures::fixture_plan::FixturePlan;
use crate::fixtures::scope_layer::ScopeLayer;

/// Resolves a test's fixture closure into an executable [`FixturePlan`].
pub trait FixtureResolver {
    /// Compute the resolved closure for the test identified by its requested fixtures + location.
    fn resolve(
        &self,
        graph: &FixtureGraph,
        requested: &[String],
        scope_path: &ScopePath,
    ) -> std::result::Result<FixtureClosure, FixtureError>;

    /// Bin a resolved closure into ordered scope layers (Session → Package → Module → Class),
    /// leaving Function-scope instances for `post_fork`.
    fn layer_assignment(
        &self,
        graph: &FixtureGraph,
        closure: &FixtureClosure,
        scope_path: &ScopePath,
    ) -> std::result::Result<Vec<ScopeLayer>, FixtureError>;

    /// Produce the full plan for a test: layers + `fork_from` + `post_fork` + `fixture_args` +
    /// `closure_hash`.
    fn plan_for(
        &self,
        graph: &FixtureGraph,
        requested: &[String],
        scope_path: &ScopePath,
    ) -> std::result::Result<FixturePlan, FixtureError>;
}
