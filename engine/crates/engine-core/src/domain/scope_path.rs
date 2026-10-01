use serde::{Deserialize, Serialize};

/// Where a test sits in the module/class hierarchy — used for snapshot-layer locality (Phase 3+).
/// Phase 2 populates `module` (and `class` for class-based tests).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopePath {
    /// Dotted-or-slashed module identifier (e.g. `pkg/test_mod.py`).
    pub module: String,
    /// Enclosing class name for class-based tests, else `None`.
    pub class: Option<String>,
}

impl ScopePath {
    pub fn module(module: impl Into<String>) -> Self {
        Self {
            module: module.into(),
            class: None,
        }
    }

    pub fn with_class(module: impl Into<String>, class: impl Into<String>) -> Self {
        Self {
            module: module.into(),
            class: Some(class.into()),
        }
    }

    /// If this location is a (possibly equal) prefix of `other`, its length in segments.
    ///
    /// Measured at path-segment granularity over the module identifier, with `/` and `.` both
    /// separators: `pkg` is a prefix of `pkg/sub/test.py`, `pk` is not. The session root (`""`)
    /// is a prefix of everything, at length 0. The class is not part of the comparison: a
    /// definition applies to a location by where its module sits, as a conftest does.
    pub fn is_prefix_of(&self, other: &ScopePath) -> Option<usize> {
        Self::module_prefix_len(&self.module, &other.module)
    }

    /// [`is_prefix_of`](Self::is_prefix_of) on two module identifiers.
    pub fn module_prefix_len(candidate: &str, target: &str) -> Option<usize> {
        fn segs(m: &str) -> Vec<&str> {
            m.split(['/', '.']).filter(|s| !s.is_empty()).collect()
        }
        let cand = segs(candidate);
        let targ = segs(target);
        if cand.len() > targ.len() {
            return None;
        }
        cand.iter()
            .zip(&targ)
            .all(|(c, t)| c == t)
            .then_some(cand.len())
    }
}

#[cfg(test)]
mod tests {
    use super::ScopePath;

    #[test]
    fn prefix_is_segment_wise_and_the_root_matches_everything() {
        let t = ScopePath::module("pkg/sub/test_x.py");
        assert_eq!(ScopePath::module("pkg").is_prefix_of(&t), Some(1));
        assert_eq!(ScopePath::module("pkg.sub").is_prefix_of(&t), Some(2));
        assert_eq!(ScopePath::module("pk").is_prefix_of(&t), None);
        assert_eq!(ScopePath::module("").is_prefix_of(&t), Some(0));
        assert_eq!(t.is_prefix_of(&t), Some(4)); // `.py` is a segment too
        assert_eq!(
            ScopePath::module("pkg/sub/test_x.py/deeper").is_prefix_of(&t),
            None
        );
    }
}
