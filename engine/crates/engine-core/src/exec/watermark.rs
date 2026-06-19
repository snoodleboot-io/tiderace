//! The `Watermark` — a forkable point in wellspring interpreter memory (design 05 §3).
//!
//! A `Watermark` is **not a copy** of memory: on Linux/macOS it records "fork the wellspring *now*,
//! after these fixtures ran." The fixture graph decides *which* scopes get one; the wellspring mints
//! the actual `Watermark` as it advances through the layers. Pure data + accessors — fully defined.

use serde::{Deserialize, Serialize};

use crate::domain::{Scope, ScopePath};

/// Stable identifier for a snapshot layer. A [`crate::fixtures::ScopeLayer`] references a snapshot by
/// `WatermarkId` (not by the full `Watermark`) so the *plan* produced by the fixture resolver stays
/// decoupled from live wellspring runtime state (pid, rss). The wellspring mints the `Watermark`
/// carrying this id when it materializes the layer.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct WatermarkId(u64);

impl WatermarkId {
    /// Construct a watermark id from a raw monotonic counter value.
    pub fn new(raw: u64) -> Self {
        Self(raw)
    }

    /// The raw id value.
    pub fn get(self) -> u64 {
        self.0
    }
}

/// A snapshot layer marker: a forkable point in wellspring memory after a scope's fixtures ran.
///
/// `rss_bytes` is the load-bearing input to the Phase 6 `MemoryGovernor` (seeds `per_fork_estimate`,
/// design 05 §6.3). `is_live` distinguishes a minted-and-current layer from one that has been
/// invalidated/retired (the `WatermarkStack` flips it; see W9).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Watermark {
    /// Stable id; referenced by `ScopeLayer.snapshot` and `ForkPlan.fork_from`.
    pub id: WatermarkId,
    /// The scope this layer captures (Session/Package/Module/Class — never Function).
    pub scope: Scope,
    /// The location the layer was minted for (which module/package/class).
    pub scope_path: ScopePath,
    /// Resident set size of the wellspring at the moment the layer was minted; the governor's seed.
    pub rss_bytes: u64,
    /// The wellspring process that owns this fork point.
    pub wellspring_pid: i64,
    /// `false` once the layer has been invalidated or retired (no longer a valid fork source).
    pub is_live: bool,
}

impl Watermark {
    /// Mint a live watermark for `scope` at `scope_path`.
    pub fn new(
        id: WatermarkId,
        scope: Scope,
        scope_path: ScopePath,
        rss_bytes: u64,
        wellspring_pid: i64,
    ) -> Self {
        Self {
            id,
            scope,
            scope_path,
            rss_bytes,
            wellspring_pid,
            is_live: true,
        }
    }

    /// The watermark's id.
    pub fn id(&self) -> WatermarkId {
        self.id.clone()
    }

    /// The scope this layer captures.
    pub fn scope(&self) -> Scope {
        self.scope
    }

    /// Whether this watermark is still a valid fork source.
    pub fn is_live(&self) -> bool {
        self.is_live
    }
}
