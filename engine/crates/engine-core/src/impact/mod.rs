//! Impact analysis (Phase 5, design 11) — select tests by *changed files × [`DepGraph`]*.
//!
//! A precise, line-level (not file-only) impact model: because coverage is always on (ADR-E006), the
//! [`DepGraph`] is always current, so a warm run with no changes skips
//! every test and a single edit re-runs only the tests whose footprint touches it.
//!
//! One type per file (ADR-E005): [`Change`], [`Selection`], [`ImpactAnalyzer`].
//!
//! [`DepGraph`]: crate::coverage::DepGraph

mod change;
mod impact_analyzer;
mod selection;

pub use change::Change;
pub use impact_analyzer::ImpactAnalyzer;
pub use selection::Selection;
