//! Native fixture dependency injection — pytest's fixture model reimplemented in Rust, with no
//! pytest underneath (ADR-E001). The subsystem is both the user-facing DI contract and the structure
//! that decides what is baked into a memory snapshot vs run fresh in a forked child (ADR-E003).
//!
//! See `planning/current/pure-rust-test-engine/design/04-fixture-graph.md` and the Phase 3
//! `CONTRACT.md`. One public type per file (per project conventions).
//!
//! **Contract-freeze note.** This `mod.rs` is owned by the contract step (architect-agent), not by
//! any lane — lanes only *overwrite their owned files*, never this module wiring. Pure-data types
//! (`Fixture`, `FixtureInstance`, `ScopeLayer`, `FixturePlan`, `Finalizer`, …) are fully defined;
//! behaviour seams (`FixtureGraph`, `LayeredResolver`, `OverrideTable`) carry clearly-marked
//! `unimplemented!("LANE: …")` scaffolds the lanes replace.

mod closure_hash;
mod finalizer;
mod fixture;
mod fixture_args;
mod fixture_closure;
mod fixture_error;
mod fixture_graph;
mod fixture_instance;
mod fixture_plan;
mod fixture_resolver;
mod layered_resolver;
mod override_table;
mod param_value;
mod scope_layer;
mod shim_handle;

pub use closure_hash::{ClosureHash, ClosureHasher};
pub use finalizer::Finalizer;
pub use fixture::Fixture;
pub use fixture_args::FixtureArgs;
pub use fixture_closure::FixtureClosure;
pub use fixture_error::FixtureError;
pub use fixture_graph::FixtureGraph;
pub use fixture_instance::FixtureInstance;
pub use fixture_plan::FixturePlan;
pub use fixture_resolver::FixtureResolver;
pub use layered_resolver::LayeredResolver;
pub use override_table::OverrideTable;
pub use param_value::ParamValue;
pub use scope_layer::ScopeLayer;
pub use shim_handle::ShimHandle;
