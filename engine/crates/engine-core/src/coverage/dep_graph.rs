use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use crate::coverage::{CoverageReport, FileLines};
use crate::domain::NodeId;

/// The dependency graph: source file → the tests that touch it (and which lines), built from per-test
/// [`CoverageReport`]s (design 11). It is the engine's always-on dependency tracker — the single
/// structure that feeds **both** impact selection (which tests a change affects) and the
/// content-addressed cache key's `executed_sources` soundness term (ADR-E004/E006).
#[derive(Debug, Default)]
pub struct DepGraph {
    /// test → its full touched footprint (for cache-key building + introspection).
    edges: HashMap<NodeId, Vec<FileLines>>,
    /// source file → tests that touched it (the reverse index impact selection walks).
    by_file: HashMap<PathBuf, BTreeSet<NodeId>>,
}

impl DepGraph {
    /// An empty graph.
    pub fn new() -> Self {
        Self::default()
    }

    /// Fold one test's coverage into the graph, replacing any prior footprint for that node (a
    /// re-run supersedes the stale edges so the reverse index never accumulates dead entries).
    pub fn record(&mut self, report: CoverageReport) {
        let node = report.node_id().clone();
        if let Some(old) = self.edges.remove(&node) {
            for fl in &old {
                if let Some(set) = self.by_file.get_mut(fl.source_path()) {
                    set.remove(&node);
                    if set.is_empty() {
                        self.by_file.remove(fl.source_path());
                    }
                }
            }
        }
        let touched = report.touched().to_vec();
        for fl in &touched {
            self.by_file
                .entry(fl.source_path().to_path_buf())
                .or_default()
                .insert(node.clone());
        }
        self.edges.insert(node, touched);
    }

    /// The touched footprint of one test (its source dependencies), or empty if unseen.
    pub fn deps_of(&self, node: &NodeId) -> &[FileLines] {
        self.edges.get(node).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Every test that touched `path` (file-level impact: any change to the file).
    pub fn tests_touching(&self, path: &Path) -> Vec<NodeId> {
        self.by_file
            .get(path)
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Every test whose footprint in `path` intersects the `changed` line set (line-level impact —
    /// the precise selection that re-runs only tests a specific edit actually affects).
    pub fn tests_touching_lines(&self, path: &Path, changed: &BTreeSet<u32>) -> Vec<NodeId> {
        let Some(candidates) = self.by_file.get(path) else {
            return Vec::new();
        };
        candidates
            .iter()
            .filter(|node| {
                self.edges
                    .get(*node)
                    .into_iter()
                    .flatten()
                    .filter(|fl| fl.source_path() == path)
                    .any(|fl| fl.intersects(changed))
            })
            .cloned()
            .collect()
    }

    /// The number of tests recorded.
    pub fn len(&self) -> usize {
        self.edges.len()
    }

    /// Whether the graph holds no tests.
    pub fn is_empty(&self) -> bool {
        self.edges.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn report(node: &str, file: &str, lines: &[u32]) -> CoverageReport {
        let mut wire = BTreeMap::new();
        wire.insert(file.to_string(), lines.to_vec());
        CoverageReport::from_wire(NodeId::new(node), wire)
    }

    #[test]
    fn records_forward_and_reverse_edges() {
        let mut g = DepGraph::new();
        g.record(report("t.py::a", "src.py", &[1, 2, 3]));
        g.record(report("t.py::b", "src.py", &[10]));

        assert_eq!(g.len(), 2);
        let mut touching = g.tests_touching(Path::new("src.py"));
        touching.sort();
        assert_eq!(
            touching,
            vec![NodeId::new("t.py::a"), NodeId::new("t.py::b")]
        );
        assert_eq!(
            g.deps_of(&NodeId::new("t.py::a"))[0].source_path(),
            Path::new("src.py")
        );
    }

    #[test]
    fn line_level_selection_is_precise() {
        let mut g = DepGraph::new();
        g.record(report("t.py::a", "src.py", &[1, 2, 3]));
        g.record(report("t.py::b", "src.py", &[10, 11]));

        // A change on line 2 hits only test a; line 10 hits only test b.
        let l2: BTreeSet<u32> = [2].into_iter().collect();
        assert_eq!(
            g.tests_touching_lines(Path::new("src.py"), &l2),
            vec![NodeId::new("t.py::a")]
        );
        let l10: BTreeSet<u32> = [10].into_iter().collect();
        assert_eq!(
            g.tests_touching_lines(Path::new("src.py"), &l10),
            vec![NodeId::new("t.py::b")]
        );
        // A line nobody touched selects no one.
        let l99: BTreeSet<u32> = [99].into_iter().collect();
        assert!(g.tests_touching_lines(Path::new("src.py"), &l99).is_empty());
    }

    #[test]
    fn re_record_supersedes_stale_reverse_edges() {
        let mut g = DepGraph::new();
        g.record(report("t.py::a", "old.py", &[1]));
        g.record(report("t.py::a", "new.py", &[1])); // a no longer touches old.py
        assert!(g.tests_touching(Path::new("old.py")).is_empty());
        assert_eq!(
            g.tests_touching(Path::new("new.py")),
            vec![NodeId::new("t.py::a")]
        );
        assert_eq!(g.len(), 1);
    }
}
