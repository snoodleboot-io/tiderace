use std::collections::HashMap;
use std::sync::Mutex;

use crate::cache::{Cache, CacheKey, CachedOutcome};

/// The local cache tier: an in-process content store keyed by [`CacheKey`]. Backed by a `Mutex`-guarded
/// map so it satisfies the `Cache: Send + Sync` seam and can be shared across the worker pool.
///
/// ADR-E004 specifies a content store + SQLite index on disk for cross-run persistence; this in-memory
/// store is the faithful `Cache` impl for the warm/inner-loop path and the unit substrate. SQLite-backed
/// persistence is a drop-in behind the same trait (the `persist`/`load` follow-on).
#[derive(Debug, Default)]
pub struct LocalCache {
    entries: Mutex<HashMap<CacheKey, CachedOutcome>>,
}

impl LocalCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// The number of cached entries (diagnostics/tests).
    pub fn len(&self) -> usize {
        self.entries.lock().expect("cache mutex").len()
    }

    /// Whether the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.lock().expect("cache mutex").is_empty()
    }
}

impl Cache for LocalCache {
    fn get(&self, key: &CacheKey) -> Option<CachedOutcome> {
        self.entries.lock().expect("cache mutex").get(key).cloned()
    }

    fn put(&self, key: &CacheKey, outcome: CachedOutcome) {
        self.entries
            .lock()
            .expect("cache mutex")
            .insert(*key, outcome);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::CacheKeyBuilder;
    use crate::domain::Outcome;

    fn key(node: &str) -> CacheKey {
        CacheKeyBuilder::new(node, "0.5.0", "3.12", "linux").finish()
    }

    #[test]
    fn miss_then_hit() {
        let c = LocalCache::new();
        let k = key("t.py::a");
        assert!(c.get(&k).is_none());
        c.put(&k, CachedOutcome::new(Outcome::Passed, ""));
        assert_eq!(c.get(&k).unwrap().outcome(), Outcome::Passed);
        assert_eq!(c.len(), 1);
    }

    #[test]
    fn distinct_keys_do_not_collide() {
        let c = LocalCache::new();
        c.put(&key("t.py::a"), CachedOutcome::new(Outcome::Passed, ""));
        c.put(&key("t.py::b"), CachedOutcome::new(Outcome::Failed, "boom"));
        assert_eq!(c.get(&key("t.py::a")).unwrap().outcome(), Outcome::Passed);
        assert_eq!(c.get(&key("t.py::b")).unwrap().outcome(), Outcome::Failed);
    }
}
