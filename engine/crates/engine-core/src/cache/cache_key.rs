use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::fixtures::{ClosureHash, ClosureHasher};

/// The content-addressed cache key for one test outcome (ADR-E004): a hash over the test's
/// *transitive input closure*, so an outcome is a pure function of its inputs and shareable across
/// machines/CI. Built by [`CacheKeyBuilder`], which composes the ADR's terms:
///
/// ```text
/// key = H( test_identity + executed_source_closure   (from coverage, E006)
///          + fixture_closure (ClosureHash)           + declared_env
///          + engine_version + python_version + platform )
/// ```
///
/// Reuses the engine's existing deterministic [`ClosureHasher`] (no new crypto dependency — see its
/// module note; swap-in point is documented there if collision-resistance is later required).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CacheKey(ClosureHash);

impl CacheKey {
    /// Wrap a precomputed digest.
    pub fn from_hash(h: ClosureHash) -> Self {
        Self(h)
    }

    /// The underlying digest.
    pub fn hash(&self) -> &ClosureHash {
        &self.0
    }

    /// Lowercase-hex rendering (the index/diagnostics form).
    pub fn to_hex(&self) -> String {
        self.0.to_hex()
    }
}

/// Composes a [`CacheKey`] deterministically, independent of call order: environment terms are fed at
/// construction, the executed-source closure and declared env accumulate into sorted maps, and
/// [`finish`](Self::finish) feeds everything in a fixed order. The **executed-source closure comes
/// from coverage** (ADR-E006), not a static guess — that is what makes the cache sound.
#[derive(Debug, Clone)]
pub struct CacheKeyBuilder {
    node_id: String,
    fixture_closure: Option<ClosureHash>,
    /// relative source path -> its content hash (sorted for determinism).
    sources: BTreeMap<PathBuf, [u8; 32]>,
    /// declared env var -> value (sorted for determinism).
    env: BTreeMap<String, String>,
    engine_version: String,
    python_version: String,
    platform: String,
}

impl CacheKeyBuilder {
    /// Start a key for `node_id`, pinning the environment terms (so a cache built under one
    /// engine/python/platform never poisons another — ADR-E004 invalidation requirement).
    pub fn new(
        node_id: impl Into<String>,
        engine_version: impl Into<String>,
        python_version: impl Into<String>,
        platform: impl Into<String>,
    ) -> Self {
        Self {
            node_id: node_id.into(),
            fixture_closure: None,
            sources: BTreeMap::new(),
            env: BTreeMap::new(),
            engine_version: engine_version.into(),
            python_version: python_version.into(),
            platform: platform.into(),
        }
    }

    /// Bind the fixture-closure term (the existing [`ClosureHash`], W14).
    pub fn fixture_closure(&mut self, h: ClosureHash) -> &mut Self {
        self.fixture_closure = Some(h);
        self
    }

    /// Add one executed source file and its content hash (call once per touched file; order-free).
    pub fn executed_source(
        &mut self,
        path: impl Into<PathBuf>,
        content_hash: [u8; 32],
    ) -> &mut Self {
        self.sources.insert(path.into(), content_hash);
        self
    }

    /// Declare an env var the test is allowed to read (part of the closure; order-free).
    pub fn declared_env(&mut self, key: impl Into<String>, value: impl Into<String>) -> &mut Self {
        self.env.insert(key.into(), value.into());
        self
    }

    /// Finalize into the content-addressed [`CacheKey`].
    pub fn finish(&self) -> CacheKey {
        let mut h = ClosureHasher::new();
        // Domain-separate each section with a tag so a value can't migrate between fields.
        h.feed_str("engine").feed_str(&self.engine_version);
        h.feed_str("python").feed_str(&self.python_version);
        h.feed_str("platform").feed_str(&self.platform);
        h.feed_str("node").feed_str(&self.node_id);
        h.feed_str("fixtures").feed(
            self.fixture_closure
                .map(|c| *c.as_bytes())
                .unwrap_or([0u8; 32])
                .as_slice(),
        );
        h.feed_str("sources");
        for (path, digest) in &self.sources {
            h.feed(path_bytes(path)).feed(digest);
        }
        h.feed_str("env");
        for (key, value) in &self.env {
            h.feed_str(key).feed_str(value);
        }
        CacheKey::from_hash(h.finish())
    }
}

#[cfg(unix)]
fn path_bytes(p: &Path) -> &[u8] {
    use std::os::unix::ffi::OsStrExt;
    p.as_os_str().as_bytes()
}

#[cfg(not(unix))]
fn path_bytes(p: &Path) -> &[u8] {
    // Non-unix: lossy is acceptable — the digest just needs to be stable per-platform, and the
    // platform term already partitions keys across OSes.
    p.to_str().unwrap_or("").as_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> CacheKeyBuilder {
        CacheKeyBuilder::new("t.py::a", "0.5.0", "3.12.3", "linux-x86_64")
    }

    #[test]
    fn identical_closures_yield_equal_keys() {
        let a = base().executed_source("src.py", [7u8; 32]).finish();
        let b = base().executed_source("src.py", [7u8; 32]).finish();
        assert_eq!(a, b);
    }

    #[test]
    fn source_content_change_changes_key() {
        let a = base().executed_source("src.py", [7u8; 32]).finish();
        let b = base().executed_source("src.py", [8u8; 32]).finish();
        assert_ne!(a, b, "a changed source must miss the cache");
    }

    #[test]
    fn env_terms_partition_keys() {
        let linux = CacheKeyBuilder::new("t.py::a", "0.5.0", "3.12.3", "linux").finish();
        let mac = CacheKeyBuilder::new("t.py::a", "0.5.0", "3.12.3", "macos").finish();
        assert_ne!(
            linux, mac,
            "platform must partition the cache (no cross-env poisoning)"
        );
    }

    #[test]
    fn source_order_does_not_matter() {
        let mut a = base();
        a.executed_source("a.py", [1u8; 32])
            .executed_source("b.py", [2u8; 32]);
        let mut b = base();
        b.executed_source("b.py", [2u8; 32])
            .executed_source("a.py", [1u8; 32]);
        assert_eq!(a.finish(), b.finish());
    }

    #[test]
    fn fixture_closure_term_participates() {
        let a = base().finish();
        let b = base()
            .fixture_closure(ClosureHash::from_bytes([9u8; 32]))
            .finish();
        assert_ne!(a, b);
    }
}
