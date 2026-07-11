use crate::domain::NodeId;

/// The result of impact analysis: which collected tests must run and which are provably unaffected.
/// `skipped` tests are safe to serve from cache (their footprint did not intersect any change).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Selection {
    pub selected: Vec<NodeId>,
    pub skipped: Vec<NodeId>,
}

impl Selection {
    /// How many tests were selected to run.
    pub fn selected_count(&self) -> usize {
        self.selected.len()
    }

    /// How many tests were skipped (unaffected by the changes).
    pub fn skipped_count(&self) -> usize {
        self.skipped.len()
    }
}
