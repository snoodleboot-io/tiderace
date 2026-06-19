//! `WatermarkStack` (W9) — the session→package→module→class layer stack the whole thesis rides on
//! (design 05 §3).
//!
//! Layers are **append-only and scope-monotonic**: a snapshot at scope *s* contains all state from
//! every wider scope, and no `Function` state ever enters a snapshot (guaranteed upstream by the
//! fixture graph's scope-monotonicity invariant). The stack answers `deepest_shared(plan)` — the
//! narrowest-scoped live snapshot shared by a test — which is exactly the fork point.
//!
//! **Contract seam.** Struct shape + method signatures frozen here; the stack mechanics
//! (`ensure_layer`/`snapshot`/`invalidate_from`/`retire_layer`) are implemented by Lane WM
//! (subagent wm-stack), editing `wellspring.rs` alongside.

use crate::domain::Scope;
use crate::exec::watermark::{Watermark, WatermarkId};
use crate::fixtures::FixturePlan;

/// The ordered stack of live snapshot layers in a wellspring lineage.
#[derive(Debug, Default)]
pub struct WatermarkStack {
    /// Layers in scope order (widest first); append-only within a wellspring's life.
    layers: Vec<Watermark>,
}

impl WatermarkStack {
    /// An empty stack (no layers minted yet).
    pub fn new() -> Self {
        Self { layers: Vec::new() }
    }

    /// All live layers, widest → narrowest.
    ///
    /// Defined (trivial accessor) so Lane WM and the governor read layer `rss_bytes` without a
    /// scaffold.
    pub fn layers(&self) -> &[Watermark] {
        &self.layers
    }

    /// The deepest (narrowest-scoped) **live** snapshot shared by `plan` — the fork point
    /// (`ForkPlan::from` selects this). `None` if no wider-than-Function layer applies (fork from
    /// the wellspring base).
    ///
    /// LANE: Lane WM (wm-stack) implements deepest_shared — W9/W10.
    pub fn deepest_shared(&self, _plan: &FixturePlan) -> Option<&Watermark> {
        unimplemented!(
            "LANE: Lane WM (wm-stack) implements WatermarkStack::deepest_shared — W9/W10"
        )
    }

    /// Append a freshly-minted layer for `scope` (must be scope-monotonic w.r.t. the current top).
    ///
    /// LANE: Lane WM (wm-stack) implements push_layer — W9.
    pub fn push_layer(&mut self, _layer: Watermark) {
        unimplemented!("LANE: Lane WM (wm-stack) implements WatermarkStack::push_layer — W9")
    }

    /// Invalidate every layer at or narrower than `scope` (content changed; mark `is_live = false`).
    ///
    /// LANE: Lane WM (wm-stack) implements invalidate_from — W9.
    pub fn invalidate_from(&mut self, _scope: Scope) {
        unimplemented!("LANE: Lane WM (wm-stack) implements WatermarkStack::invalidate_from — W9")
    }

    /// Retire a specific layer (its scope's finalizers run **once** at this point).
    ///
    /// LANE: Lane WM (wm-stack) implements retire_layer — W9.
    pub fn retire_layer(&mut self, _id: &WatermarkId) {
        unimplemented!("LANE: Lane WM (wm-stack) implements WatermarkStack::retire_layer — W9")
    }
}
