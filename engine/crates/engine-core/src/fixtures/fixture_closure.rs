//! `FixtureClosure` — the transitive set of fixtures a `TestItem` needs (requested + autouse +
//! their transitive deps), in topo (setup) order (design 04 §2.1.5).
//!
//! Produced by `FixtureGraph::closure_of`; consumed by the `LayeredResolver` to bin instances into
//! scope layers. Pure data — fully defined. (The *computation* is Lane FX-graph's job.)

use crate::domain::NodeId;

/// A test's transitive fixture closure, topologically ordered for setup.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FixtureClosure {
    /// Fixture definition node ids in setup (topo) order; teardown is the strict reverse.
    pub ordered: Vec<NodeId>,
}

impl FixtureClosure {
    /// Construct a closure from a topo-ordered list of fixture node ids.
    pub fn new(ordered: Vec<NodeId>) -> Self {
        Self { ordered }
    }

    /// The fixtures in setup order.
    pub fn setup_order(&self) -> &[NodeId] {
        &self.ordered
    }

    /// The fixtures in teardown order (the strict reverse of setup).
    pub fn teardown_order(&self) -> impl Iterator<Item = &NodeId> {
        self.ordered.iter().rev()
    }

    /// `true` if the closure is empty (the test requests no fixtures and no autouse applies).
    pub fn is_empty(&self) -> bool {
        self.ordered.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_and_teardown_are_reverses() {
        let c = FixtureClosure::new(vec![NodeId::new("a"), NodeId::new("b"), NodeId::new("c")]);
        assert_eq!(c.setup_order().len(), 3);
        let td: Vec<&str> = c.teardown_order().map(|n| n.as_str()).collect();
        assert_eq!(td, vec!["c", "b", "a"]);
        assert!(!c.is_empty());
    }

    #[test]
    fn empty_closure() {
        let c = FixtureClosure::default();
        assert!(c.is_empty());
        assert_eq!(c.teardown_order().count(), 0);
    }
}
