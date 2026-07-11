use std::collections::BTreeMap;

use crate::coverage::FileLines;
use crate::domain::NodeId;

/// One test's executed-source footprint, as reported by the shim (ADR-E006). This is the unit the
/// [`crate::coverage::DepGraph`] is built from and the `executed_sources` term the content-addressed
/// cache key (ADR-E004) consumes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoverageReport {
    node_id: NodeId,
    touched: Vec<FileLines>,
}

impl CoverageReport {
    /// Build from the wire shape the shim emits: `relative_path -> sorted line numbers`.
    pub fn from_wire(node_id: NodeId, wire: BTreeMap<String, Vec<u32>>) -> Self {
        let touched = wire
            .into_iter()
            .map(|(path, lines)| FileLines::new(path, lines))
            .collect();
        Self { node_id, touched }
    }

    /// The test this footprint belongs to.
    pub fn node_id(&self) -> &NodeId {
        &self.node_id
    }

    /// The per-file touched-line sets.
    pub fn touched(&self) -> &[FileLines] {
        &self.touched
    }

    /// Whether this test touched any source at all (an empty report ⇒ capture was off, or a no-op test).
    pub fn is_empty(&self) -> bool {
        self.touched.is_empty()
    }
}
