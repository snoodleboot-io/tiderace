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
        Self::file_of(&self.0)
    }

    /// The `::`-separated segments after the file (class / method / func / param-id).
    pub fn segments(&self) -> impl Iterator<Item = &str> {
        self.0.split("::").skip(1)
    }

    /// The id without its parametrize case: `m.py::t[1-2]` → `m.py::t`. The `[` is looked for
    /// in the last segment only, so a `[` in a class or file name is left alone.
    pub fn bare(&self) -> &str {
        Self::bare_of(&self.0)
    }

    /// The parametrize case id, if any: `m.py::t[1-2]` → `1-2`.
    pub fn param_id(&self) -> Option<&str> {
        Self::param_id_of(&self.0)
    }

    /// The owner one level up: a method's class, a function's or class's file. `None` for a bare
    /// file.
    pub fn parent(&self) -> Option<&str> {
        Self::parent_of(&self.0)
    }

    /// Whether this id is a runtime expansion of `owner` — its parametrize case (`owner[…]`) or
    /// a method it owns (`owner::…`) — and not `owner` itself, nor a sibling that merely shares
    /// a prefix (`test_ab[1]` is not an expansion of `test_a`).
    pub fn is_expansion_of(&self, owner: &str) -> bool {
        Self::expands(&self.0, owner)
    }

    // The same operations on a raw id, for the many places that hold ids as `String` keys
    // (verdict records, RPC payloads) rather than as `NodeId`.

    /// [`NodeId::file`] on a raw id.
    pub fn file_of(raw: &str) -> &str {
        raw.split("::").next().unwrap_or(raw)
    }

    /// [`NodeId::bare`] on a raw id.
    pub fn bare_of(raw: &str) -> &str {
        let last = raw.rfind("::").map_or(0, |i| i + 2);
        match raw[last..].find('[') {
            Some(b) => &raw[..last + b],
            None => raw,
        }
    }

    /// [`NodeId::param_id`] on a raw id.
    pub fn param_id_of(raw: &str) -> Option<&str> {
        let bare = Self::bare_of(raw);
        let rest = &raw[bare.len()..];
        rest.strip_prefix('[')
            .and_then(|r| r.strip_suffix(']'))
            .or(if rest.is_empty() {
                None
            } else {
                Some(&rest[1..])
            })
    }

    /// [`NodeId::parent`] on a raw id.
    pub fn parent_of(raw: &str) -> Option<&str> {
        raw.rfind("::").map(|i| &raw[..i])
    }

    /// [`NodeId::is_expansion_of`] on a raw id.
    pub fn expands(raw: &str, owner: &str) -> bool {
        raw.len() > owner.len()
            && raw.starts_with(owner)
            && (raw.as_bytes()[owner.len()] == b'[' || raw[owner.len()..].starts_with("::"))
    }
}

impl std::borrow::Borrow<str> for NodeId {
    fn borrow(&self) -> &str {
        &self.0
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

    #[test]
    fn bare_strips_only_the_last_segments_case() {
        assert_eq!(NodeId::new("m.py::t[1-2]").bare(), "m.py::t");
        assert_eq!(NodeId::new("m.py::C::t[x]").bare(), "m.py::C::t");
        assert_eq!(NodeId::new("m.py::t").bare(), "m.py::t");
        assert_eq!(NodeId::new("a[b]/m.py::t").bare(), "a[b]/m.py::t");
        assert_eq!(NodeId::new("m.py::t[1-2]").param_id(), Some("1-2"));
        assert_eq!(NodeId::new("m.py::t").param_id(), None);
    }

    #[test]
    fn parent_is_one_level_up() {
        assert_eq!(NodeId::new("m.py::C::t").parent(), Some("m.py::C"));
        assert_eq!(NodeId::new("m.py::t").parent(), Some("m.py"));
        assert_eq!(NodeId::new("m.py").parent(), None);
    }

    #[test]
    fn expansion_is_a_case_or_a_method_never_a_sibling_or_itself() {
        assert!(NodeId::new("m.py::t[1]").is_expansion_of("m.py::t"));
        assert!(NodeId::new("m.py::C::t").is_expansion_of("m.py::C"));
        assert!(!NodeId::new("m.py::t").is_expansion_of("m.py::t"));
        assert!(!NodeId::new("m.py::test_ab[1]").is_expansion_of("m.py::test_a"));
        assert!(!NodeId::new("m.py::test_ab").is_expansion_of("m.py::test_a"));
    }

    #[test]
    fn a_set_of_ids_is_looked_up_by_str() {
        let set: std::collections::HashSet<NodeId> = [NodeId::new("m.py::t")].into();
        assert!(set.contains("m.py::t"));
        assert_eq!(NodeId::file_of("pkg/m.py::C::t"), "pkg/m.py");
        assert_eq!(NodeId::file_of("bare"), "bare");
    }
}
