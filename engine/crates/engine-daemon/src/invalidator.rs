use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// What a file change requires of the warm daemon (ADR-E007's "robust module invalidation" — the
/// classic stale-import failure mode). Decided by **content hash**, not mtime, so an editor that
/// rewrites a file with identical bytes triggers nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invalidation {
    /// Content identical to what we last saw — a no-op (mtime-only touch).
    Unchanged,
    /// Not a file the engine cares about.
    Ignored,
    /// `conftest.py`, project config, or a C-extension changed → the warm interpreter's imported
    /// state is stale; the wellspring must be **recycled** (respawned), not patched.
    RecycleWellspring(String),
    /// A test file changed → re-collect it (tests may have been added/removed/renamed).
    Recollect(PathBuf),
    /// A non-test source file changed → run impact analysis (the tests that touched it).
    SourceChanged(PathBuf),
}

/// Tracks per-file content hashes and classifies each change (design 08 `invalidator.rs`). Recycling
/// on `conftest`/config/C-ext is the conservative correctness guard; everything else is incremental.
#[derive(Debug, Default)]
pub struct Invalidator {
    hashes: HashMap<PathBuf, [u8; 32]>,
}

impl Invalidator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a file's current hash without classifying (initial baseline at collection time).
    pub fn seed(&mut self, path: impl Into<PathBuf>, hash: [u8; 32]) {
        self.hashes.insert(path.into(), hash);
    }

    /// The last-seen hash of `path` (for cache-key building).
    pub fn hash_of(&self, path: &Path) -> Option<[u8; 32]> {
        self.hashes.get(path).copied()
    }

    /// Classify a change. Updates the stored hash when the content actually changed.
    pub fn observe(&mut self, path: impl AsRef<Path>, new_hash: [u8; 32]) -> Invalidation {
        let path = path.as_ref();
        let kind = classify(path);
        if matches!(kind, Kind::Ignored) {
            return Invalidation::Ignored;
        }
        if self.hashes.get(path) == Some(&new_hash) {
            return Invalidation::Unchanged; // content-identical → no spurious rerun
        }
        self.hashes.insert(path.to_path_buf(), new_hash);
        match kind {
            Kind::Recycle => Invalidation::RecycleWellspring(recycle_reason(path)),
            Kind::Test => Invalidation::Recollect(path.to_path_buf()),
            Kind::Source => Invalidation::SourceChanged(path.to_path_buf()),
            Kind::Ignored => unreachable!(),
        }
    }
}

enum Kind {
    Recycle,
    Test,
    Source,
    Ignored,
}

const CONFIG_FILES: &[&str] = &[
    "pyproject.toml",
    "setup.cfg",
    "setup.py",
    "tox.ini",
    "conftest.py",
];

fn classify(path: &Path) -> Kind {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if CONFIG_FILES.contains(&name) {
        return Kind::Recycle;
    }
    match path.extension().and_then(|e| e.to_str()) {
        Some("so") | Some("pyd") | Some("dylib") => Kind::Recycle, // C-ext: in-interpreter state stale
        Some("py") => {
            if name.starts_with("test_") || name.ends_with("_test.py") {
                Kind::Test
            } else {
                Kind::Source
            }
        }
        _ => Kind::Ignored,
    }
}

fn recycle_reason(path: &Path) -> String {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if name == "conftest.py" {
        "conftest changed".into()
    } else if matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("so" | "pyd" | "dylib")
    ) {
        "C-extension changed".into()
    } else {
        "project config changed".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unchanged_content_is_a_no_op() {
        let mut inv = Invalidator::new();
        inv.seed("tests/test_a.py", [1; 32]);
        assert_eq!(
            inv.observe("tests/test_a.py", [1; 32]),
            Invalidation::Unchanged
        );
    }

    #[test]
    fn test_file_change_recollects() {
        let mut inv = Invalidator::new();
        inv.seed("tests/test_a.py", [1; 32]);
        assert_eq!(
            inv.observe("tests/test_a.py", [2; 32]),
            Invalidation::Recollect("tests/test_a.py".into())
        );
    }

    #[test]
    fn source_change_triggers_impact() {
        let mut inv = Invalidator::new();
        assert_eq!(
            inv.observe("src/mod.py", [9; 32]),
            Invalidation::SourceChanged("src/mod.py".into())
        );
    }

    #[test]
    fn conftest_config_and_cext_recycle_the_wellspring() {
        let mut inv = Invalidator::new();
        assert!(matches!(
            inv.observe("conftest.py", [1; 32]),
            Invalidation::RecycleWellspring(_)
        ));
        assert!(matches!(
            inv.observe("pyproject.toml", [1; 32]),
            Invalidation::RecycleWellspring(_)
        ));
        assert!(matches!(
            inv.observe("pkg/_speedups.so", [1; 32]),
            Invalidation::RecycleWellspring(_)
        ));
    }

    #[test]
    fn non_python_is_ignored() {
        let mut inv = Invalidator::new();
        assert_eq!(inv.observe("README.md", [1; 32]), Invalidation::Ignored);
    }
}
