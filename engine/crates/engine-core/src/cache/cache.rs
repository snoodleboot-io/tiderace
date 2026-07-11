use crate::cache::{CacheKey, CachedOutcome};

/// The cache seam (ADR-E005): a content-addressed store of test outcomes. Production wires a
/// [`TieredCache`](crate::cache::TieredCache) (local + optional remote); `--no-cache` / debugging
/// wires a [`NullCache`](crate::cache::NullCache).
///
/// Implementors use interior mutability (`&self` on `put`) so a single cache can be shared across the
/// worker pool without a `&mut` bottleneck. The orchestrator's preference order is **cache hit →
/// impact-skip → run** (ADR-E004): consult [`get`](Self::get) first, fall back to impact selection,
/// then execute and [`put`](Self::put) the fresh outcome.
pub trait Cache: Send + Sync {
    /// The cached outcome for `key`, or `None` on a miss.
    fn get(&self, key: &CacheKey) -> Option<CachedOutcome>;

    /// Store `outcome` under `key`. Callers must only store **pure** outcomes
    /// (`Purity::is_cacheable`) — impure tests are never silently cached (ADR-E004 soundness).
    fn put(&self, key: &CacheKey, outcome: CachedOutcome);
}
