use crate::cache::{Cache, CacheKey, CachedOutcome};

/// A cache that stores nothing and always misses (ADR-E005). Wired by `--no-cache` and used in
/// debugging / differential runs where every test must actually execute. Lets the orchestrator depend
/// on the [`Cache`] seam unconditionally instead of branching on an `Option<Cache>`.
#[derive(Debug, Default, Clone, Copy)]
pub struct NullCache;

impl NullCache {
    pub fn new() -> Self {
        Self
    }
}

impl Cache for NullCache {
    fn get(&self, _key: &CacheKey) -> Option<CachedOutcome> {
        None
    }

    fn put(&self, _key: &CacheKey, _outcome: CachedOutcome) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::CacheKeyBuilder;
    use crate::domain::Outcome;

    #[test]
    fn never_stores() {
        let c = NullCache::new();
        let k = CacheKeyBuilder::new("t.py::a", "0.5.0", "3.12", "linux").finish();
        c.put(&k, CachedOutcome::new(Outcome::Passed, ""));
        assert!(c.get(&k).is_none());
    }
}
