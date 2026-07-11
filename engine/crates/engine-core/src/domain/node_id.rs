use std::fmt;

use serde::{Deserialize, Serialize};

/// A pytest-compatible test node id, e.g. `pkg/test_mod.py::Class::method` or
/// `pkg/test_mod.py::func`. The universal currency for selection, results, and (later) caching.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct NodeId(String);

impl NodeId {
    pub fn new(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The file part (everything before the first `::`).
    pub fn file(&self) -> &str {
        self.0.split("::").next().unwrap_or(&self.0)
    }

    /// The `::`-separated segments after the file (class / method / func / param-id).
    pub fn segments(&self) -> impl Iterator<Item = &str> {
        self.0.split("::").skip(1)
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_is_text_before_first_separator() {
        let id = NodeId::new("pkg/test_mod.py::Case::test_x");
        assert_eq!(id.file(), "pkg/test_mod.py");
    }

    #[test]
    fn segments_skip_the_file() {
        let id = NodeId::new("test_mod.py::Case::test_x");
        assert_eq!(id.segments().collect::<Vec<_>>(), vec!["Case", "test_x"]);
    }

    #[test]
    fn function_node_has_one_segment() {
        let id = NodeId::new("test_mod.py::test_x");
        assert_eq!(id.segments().collect::<Vec<_>>(), vec!["test_x"]);
    }
}
