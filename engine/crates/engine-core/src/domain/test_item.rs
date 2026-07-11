use super::{NodeId, ScopePath, TestStyle};

/// A discovered test — produced by [`crate::collection`] and consumed by execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestItem {
    pub node_id: NodeId,
    pub style: TestStyle,
    pub scope_path: ScopePath,
}

impl TestItem {
    pub fn new(node_id: NodeId, style: TestStyle, scope_path: ScopePath) -> Self {
        Self {
            node_id,
            style,
            scope_path,
        }
    }
}
