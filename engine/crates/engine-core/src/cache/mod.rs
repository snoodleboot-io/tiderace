//! Content-addressed result cache (Phase 5, ADR-E004) — a build-system-for-tests.
//!
//! The fastest test is the one never run. Each outcome is modelled as a pure function of its inputs
//! and stored by content hash, so warm/CI runs approach O(changed tests) and a green result is
//! shareable across machines. The [`CacheKey`] hashes the test's transitive input closure — crucially
//! the **executed-source closure from coverage** ([`crate::coverage`]), not a static guess, which is
//! what makes the cache *sound*. Impure tests are never silently cached ([`Purity`]).
//!
//! Seams (ADR-E005): the [`Cache`] trait with [`LocalCache`] + [`TieredCache`] for CI sharing (backed
//! by a shareable [`DirCache`] remote tier) and [`NullCache`] for debugging. One type per file.
//!
//! Orchestrator preference order (ADR-E004): **cache hit → impact-skip → run**.

#[allow(clippy::module_inception)]
// file name = snake_case of the `Cache` trait (project convention)
mod cache;
mod cache_key;
mod cached_outcome;
mod dir_cache;
mod local_cache;
mod null_cache;
mod purity;
mod tiered_cache;

pub use cache::Cache;
pub use cache_key::{CacheKey, CacheKeyBuilder};
pub use cached_outcome::CachedOutcome;
pub use dir_cache::DirCache;
pub use local_cache::LocalCache;
pub use null_cache::NullCache;
pub use purity::{NoSandbox, Purity, SandboxHooks};
pub use tiered_cache::TieredCache;
