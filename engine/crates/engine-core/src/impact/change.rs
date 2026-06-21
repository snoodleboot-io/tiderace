use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// One changed source file since the last run. `lines = None` means "treat the whole file as changed"
/// (e.g. a new/deleted file, or a diff without line info); `Some(set)` enables **line-level** impact —
/// a test is only selected if its footprint actually overlaps those lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    path: PathBuf,
    lines: Option<BTreeSet<u32>>,
}

impl Change {
    /// A whole-file change (no line granularity).
    pub fn file(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            lines: None,
        }
    }

    /// A change scoped to specific lines.
    pub fn lines(path: impl Into<PathBuf>, lines: impl IntoIterator<Item = u32>) -> Self {
        Self {
            path: path.into(),
            lines: Some(lines.into_iter().collect()),
        }
    }

    /// The changed file (relative to the corpus root, matching the DepGraph key).
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The changed lines, if known.
    pub fn changed_lines(&self) -> Option<&BTreeSet<u32>> {
        self.lines.as_ref()
    }
}
