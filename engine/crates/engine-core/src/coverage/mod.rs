//! Coverage & dependency tracking (Phase 5, design 11, ADR-E006).
//!
//! Coverage here is **not** a reporting feature — it is the engine's dependency tracker. The shim
//! captures each test's executed-source footprint inside its fork child (`sys.monitoring` on 3.12+,
//! `settrace` below) and streams it back; the orchestrator folds those [`CoverageReport`]s into a
//! [`DepGraph`] (source file → tests that touch it). That one structure feeds **both** impact
//! selection ([`crate::impact`]) and the content-addressed cache key's soundness term (ADR-E004).
//!
//! One type per file (ADR-E005): [`FileLines`], [`CoverageReport`], [`DepGraph`].

mod coverage_report;
mod dep_graph;
mod file_lines;

pub use coverage_report::CoverageReport;
pub use dep_graph::DepGraph;
pub use file_lines::FileLines;
