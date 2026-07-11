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
    /// `plan.layers` are ordered widest → narrowest, so we scan them narrowest-first (`.rev()`) and
    /// return the first whose `(scope, scope_path)` matches a **live** layer in this stack. Matching
    /// on the resolved location rather than the plan's (possibly not-yet-minted) `snapshot` id keeps
    /// the answer correct whether or not the resolver backfilled watermark ids into the plan
    /// (design 05 §3.1).
    pub fn deepest_shared(&self, plan: &FixturePlan) -> Option<&Watermark> {
        for layer in plan.layers.iter().rev() {
            if let Some(wm) = self
                .layers
                .iter()
                .find(|w| w.is_live && w.scope == layer.scope && w.scope_path == layer.scope_path)
            {
                return Some(wm);
            }
        }
        None
    }

    /// Append a freshly-minted layer (W9). The stack is **append-only** within a wellspring's life;
    /// the wellspring advances through scopes widest → narrowest, so each pushed layer is the same
    /// or narrower than the previous — the scope-monotonicity invariant the snapshot stack relies on
    /// (design 05 §3). The caller (the wellspring advancing through layers) upholds the ordering.
    pub fn push_layer(&mut self, layer: Watermark) {
        self.layers.push(layer);
    }

    /// Invalidate every layer at or narrower than `scope` (its content changed): a snapshot built on
    /// top of changed wider-scope state is no longer a valid fork source. `Scope`'s derived `Ord`
    /// runs narrowest → widest, so "at or narrower than `scope`" is `layer.scope <= scope`.
    pub fn invalidate_from(&mut self, scope: Scope) {
        for layer in self.layers.iter_mut() {
            if layer.scope <= scope {
                layer.is_live = false;
            }
        }
    }

    /// Retire a specific layer — its scope's finalizers run **once** at this point (the shim invokes
    /// them; here the layer is marked no-longer-live so it is never forked from again). Append-only:
    /// the record is kept, not removed.
    pub fn retire_layer(&mut self, id: &WatermarkId) {
        for layer in self.layers.iter_mut() {
            if &layer.id == id {
                layer.is_live = false;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::ScopePath;
    use crate::exec::watermark::Watermark;
    use crate::fixtures::{ClosureHash, FixtureArgs, FixturePlan, ScopeLayer};

    fn wm(id: u64, scope: Scope, module: &str) -> Watermark {
        Watermark::new(
            WatermarkId::new(id),
            scope,
            ScopePath::module(module),
            1024,
            42,
        )
    }

    fn plan_with_layers(layers: Vec<ScopeLayer>) -> FixturePlan {
        FixturePlan::new(
            crate::domain::NodeId::new("m.py::t"),
            layers,
            None,
            Vec::new(),
            FixtureArgs::new(),
            ClosureHash::from_bytes([0u8; 32]),
        )
    }

    fn layer(scope: Scope, module: &str) -> ScopeLayer {
        ScopeLayer::new(scope, ScopePath::module(module), vec![])
    }

    #[test]
    fn push_is_append_only_and_ordered() {
        let mut stack = WatermarkStack::new();
        stack.push_layer(wm(1, Scope::Session, ""));
        stack.push_layer(wm(2, Scope::Module, "m.py"));
        assert_eq!(stack.layers().len(), 2);
        assert_eq!(stack.layers()[0].scope, Scope::Session);
        assert_eq!(stack.layers()[1].scope, Scope::Module);
    }

    #[test]
    fn deepest_shared_returns_narrowest_matching_live_layer() {
        let mut stack = WatermarkStack::new();
        stack.push_layer(wm(1, Scope::Session, ""));
        stack.push_layer(wm(2, Scope::Module, "m.py"));
        let plan = plan_with_layers(vec![
            layer(Scope::Session, ""),
            layer(Scope::Module, "m.py"),
        ]);
        let got = stack.deepest_shared(&plan).expect("a shared live layer");
        assert_eq!(
            got.scope,
            Scope::Module,
            "narrowest shared layer is the fork point"
        );
        assert_eq!(got.id, WatermarkId::new(2));
    }

    #[test]
    fn deepest_shared_skips_non_live_and_unmatched() {
        let mut stack = WatermarkStack::new();
        stack.push_layer(wm(1, Scope::Session, ""));
        stack.push_layer(wm(2, Scope::Module, "m.py"));
        stack.invalidate_from(Scope::Module); // module layer no longer live
        let plan = plan_with_layers(vec![
            layer(Scope::Session, ""),
            layer(Scope::Module, "m.py"),
        ]);
        let got = stack.deepest_shared(&plan).expect("session is still live");
        assert_eq!(
            got.scope,
            Scope::Session,
            "falls back to the next-widest live layer"
        );
    }

    #[test]
    fn deepest_shared_none_when_no_layer_applies() {
        let stack = WatermarkStack::new();
        let plan = plan_with_layers(vec![layer(Scope::Module, "m.py")]);
        assert!(
            stack.deepest_shared(&plan).is_none(),
            "empty stack → fork from base"
        );
    }

    #[test]
    fn invalidate_from_marks_scope_and_narrower_dead_keeps_wider_live() {
        let mut stack = WatermarkStack::new();
        stack.push_layer(wm(1, Scope::Session, ""));
        stack.push_layer(wm(2, Scope::Package, "pkg"));
        stack.push_layer(wm(3, Scope::Module, "m.py"));
        stack.push_layer(wm(4, Scope::Class, "m.py"));
        stack.invalidate_from(Scope::Module);
        let live: Vec<Scope> = stack
            .layers()
            .iter()
            .filter(|w| w.is_live)
            .map(|w| w.scope)
            .collect();
        assert_eq!(
            live,
            vec![Scope::Session, Scope::Package],
            "Module + Class invalidated; wider survive"
        );
    }

    #[test]
    fn retire_layer_marks_only_that_layer_dead() {
        let mut stack = WatermarkStack::new();
        stack.push_layer(wm(1, Scope::Session, ""));
        stack.push_layer(wm(2, Scope::Module, "m.py"));
        stack.retire_layer(&WatermarkId::new(2));
        assert!(stack.layers()[0].is_live);
        assert!(!stack.layers()[1].is_live);
    }
}
