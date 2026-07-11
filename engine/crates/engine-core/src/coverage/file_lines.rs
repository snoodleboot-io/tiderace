use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// One source file and the set of its lines a test touched (design 11). The path is **relative to the
/// corpus root**, exactly as the shim reports it, so it is a stable cache/DepGraph key independent of
/// where the repo is checked out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileLines {
    source_path: PathBuf,
    lines: BTreeSet<u32>,
}

impl FileLines {
    /// Build from a relative path and its touched line numbers.
    pub fn new(source_path: impl Into<PathBuf>, lines: impl IntoIterator<Item = u32>) -> Self {
        Self {
            source_path: source_path.into(),
            lines: lines.into_iter().collect(),
        }
    }

    /// The source file (relative to the corpus root).
    pub fn source_path(&self) -> &Path {
        &self.source_path
    }

    /// The touched line numbers, ascending.
    pub fn lines(&self) -> &BTreeSet<u32> {
        &self.lines
    }

    /// Whether any line in `changed` was touched by this file's footprint — the line-level impact test.
    pub fn intersects(&self, changed: &BTreeSet<u32>) -> bool {
        // Walk the smaller set against the larger for the common (small-changeset) case.
        let (small, big) = if self.lines.len() <= changed.len() {
            (&self.lines, changed)
        } else {
            (changed, &self.lines)
        };
        small.iter().any(|l| big.contains(l))
    }
}
