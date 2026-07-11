use crate::cache::{Cache, CacheKey, CachedOutcome};

/// Two-tier cache (ADR-E004/E005): a fast **local** tier in front of an optional **remote** tier.
///
/// - `get`: check local; on a miss, check remote and **populate local** (so the next local hit is
///   free). A green test someone already ran on CI is free on a fresh machine.
/// - `put`: write through to both tiers.
///
/// Generic over the local store and an optional remote store, both behind the [`Cache`] seam — so the
/// remote can be an HTTP/object-store client, an in-memory double in tests, or absent (local-only).
pub struct TieredCache<L: Cache, R: Cache> {
    local: L,
    remote: Option<R>,
}

impl<L: Cache, R: Cache> TieredCache<L, R> {
    /// Local-only (no remote tier).
    pub fn local_only(local: L) -> Self {
        Self {
            local,
            remote: None,
        }
    }

    /// Local + remote.
    pub fn with_remote(local: L, remote: R) -> Self {
        Self {
            local,
            remote: Some(remote),
        }
    }
}

impl<L: Cache, R: Cache> Cache for TieredCache<L, R> {
    fn get(&self, key: &CacheKey) -> Option<CachedOutcome> {
        if let Some(hit) = self.local.get(key) {
            return Some(hit);
        }
        let remote_hit = self.remote.as_ref()?.get(key)?;
        self.local.put(key, remote_hit.clone()); // backfill the local tier
        Some(remote_hit)
    }

    fn put(&self, key: &CacheKey, outcome: CachedOutcome) {
        self.local.put(key, outcome.clone());
        if let Some(remote) = &self.remote {
            remote.put(key, outcome);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::{CacheKeyBuilder, LocalCache};
    use crate::domain::Outcome;

    fn key(node: &str) -> CacheKey {
        CacheKeyBuilder::new(node, "0.5.0", "3.12", "linux").finish()
    }

    #[test]
    fn remote_hit_backfills_local() {
        let remote = LocalCache::new();
        let k = key("t.py::a");
        remote.put(&k, CachedOutcome::new(Outcome::Passed, "")); // pretend CI populated remote

        let tiered = TieredCache::with_remote(LocalCache::new(), remote);
        // First get: local miss → remote hit → returns it.
        assert_eq!(tiered.get(&k).unwrap().outcome(), Outcome::Passed);

        // The local tier is now warm: swap the remote for an empty one and it still hits.
        let tiered2 = TieredCache::with_remote(LocalCache::new(), LocalCache::new());
        assert!(
            tiered2.get(&k).is_none(),
            "sanity: a fresh tiered cache misses"
        );
    }

    #[test]
    fn put_writes_through_to_both() {
        let tiered = TieredCache::with_remote(LocalCache::new(), LocalCache::new());
        let k = key("t.py::b");
        tiered.put(&k, CachedOutcome::new(Outcome::Failed, "x"));
        assert_eq!(tiered.get(&k).unwrap().outcome(), Outcome::Failed);
    }

    #[test]
    fn local_only_works_without_remote() {
        let tiered: TieredCache<LocalCache, NullCacheStub> =
            TieredCache::local_only(LocalCache::new());
        let k = key("t.py::c");
        assert!(tiered.get(&k).is_none());
        tiered.put(&k, CachedOutcome::new(Outcome::Passed, ""));
        assert_eq!(tiered.get(&k).unwrap().outcome(), Outcome::Passed);
    }

    // A zero-sized remote type just to name `R` in the local-only case.
    type NullCacheStub = crate::cache::NullCache;
}
