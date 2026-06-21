use crate::coverage::DepGraph;
use crate::domain::NodeId;
use crate::impact::{Change, Selection};

/// Selects which collected tests a set of changes actually affects, using the always-on
/// [`DepGraph`] (design 11; the precise, line-level successor to the legacy file-only
/// `tiderace/impact.rs`).
///
/// Decision per candidate test:
/// 1. **Unknown** (no recorded footprint — never run with coverage, or a brand-new node): run it, to
///    establish a baseline. Conservative: an unknown test is never wrongly skipped.
/// 2. **Known**: run iff any change's file is in the test's footprint **and** (whole-file change, or
///    its changed lines overlap the footprint). Otherwise the test is provably unaffected → skip.
///
/// Consequences (the warm-run pitch): no changes ⇒ every known test is skipped; one source edit ⇒
/// only the tests whose footprint touches the edited lines re-run.
pub struct ImpactAnalyzer<'a> {
    graph: &'a DepGraph,
}

impl<'a> ImpactAnalyzer<'a> {
    pub fn new(graph: &'a DepGraph) -> Self {
        Self { graph }
    }

    /// Partition `candidates` (the full collected node set) into `(selected, skipped)` given `changes`.
    pub fn select(&self, changes: &[Change], candidates: &[NodeId]) -> Selection {
        let mut sel = Selection::default();
        for node in candidates {
            if self.should_run(node, changes) {
                sel.selected.push(node.clone());
            } else {
                sel.skipped.push(node.clone());
            }
        }
        sel
    }

    fn should_run(&self, node: &NodeId, changes: &[Change]) -> bool {
        let footprint = self.graph.deps_of(node);
        if footprint.is_empty() {
            return true; // unknown test ⇒ baseline run (never wrongly skipped)
        }
        changes.iter().any(|change| {
            footprint
                .iter()
                .filter(|fl| fl.source_path() == change.path())
                .any(|fl| match change.changed_lines() {
                    Some(lines) => fl.intersects(lines),
                    None => true, // whole-file change overlaps any footprint in that file
                })
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::coverage::CoverageReport;

    fn graph() -> DepGraph {
        let mut g = DepGraph::new();
        for (node, lines) in [("t.py::a", vec![1u32, 2, 3]), ("t.py::b", vec![10, 11])] {
            let mut wire = BTreeMap::new();
            wire.insert("src.py".to_string(), lines);
            g.record(CoverageReport::from_wire(NodeId::new(node), wire));
        }
        g
    }

    fn candidates() -> Vec<NodeId> {
        vec![NodeId::new("t.py::a"), NodeId::new("t.py::b")]
    }

    #[test]
    fn no_changes_skips_every_known_test() {
        let g = graph();
        let sel = ImpactAnalyzer::new(&g).select(&[], &candidates());
        assert_eq!(sel.selected_count(), 0);
        assert_eq!(sel.skipped_count(), 2);
    }

    #[test]
    fn line_change_runs_only_impacted() {
        let g = graph();
        let sel = ImpactAnalyzer::new(&g).select(&[Change::lines("src.py", [2])], &candidates());
        assert_eq!(sel.selected, vec![NodeId::new("t.py::a")]);
        assert_eq!(sel.skipped, vec![NodeId::new("t.py::b")]);
    }

    #[test]
    fn whole_file_change_runs_all_touching() {
        let g = graph();
        let sel = ImpactAnalyzer::new(&g).select(&[Change::file("src.py")], &candidates());
        assert_eq!(sel.selected_count(), 2);
    }

    #[test]
    fn unknown_test_is_always_selected() {
        let g = graph();
        let cands = vec![NodeId::new("t.py::a"), NodeId::new("t.py::new")];
        let sel = ImpactAnalyzer::new(&g).select(&[Change::lines("other.py", [1])], &cands);
        // a is unaffected (touches src.py, not other.py); new is unknown ⇒ baseline run.
        assert_eq!(sel.selected, vec![NodeId::new("t.py::new")]);
        assert_eq!(sel.skipped, vec![NodeId::new("t.py::a")]);
    }
}
