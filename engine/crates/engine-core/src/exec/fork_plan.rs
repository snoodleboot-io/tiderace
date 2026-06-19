//! `ForkPlan` (W10) — the executor-facing fork recipe derived from a [`FixturePlan`] + a live
//! [`WatermarkStack`] (design 05 §2, §3.1).
//!
//! `ForkPlan::from(plan, stack)` selects `fork_from = stack.deepest_shared(plan)`, carries the
//! Function-scope `post_fork` instances and the `reinit_in_child` node ids to rebuild post-fork, and
//! an estimate of COW pages the child will touch (governor input).
//!
//! **Contract seam.** Struct shape + `from` signature frozen here; the derivation logic is
//! implemented by Lane WM (subagent wm-fork), which also edits `fork_worker.rs`.

use crate::domain::NodeId;
use crate::exec::watermark::Watermark;
use crate::exec::watermark_stack::WatermarkStack;
use crate::fixtures::{FixtureInstance, FixturePlan};

/// The per-test fork recipe: where to fork from, what to set up in the child, and a COW estimate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForkPlan {
    /// The watermark to fork from (the deepest shared live snapshot), or `None` to fork the base.
    pub fork_from: Option<Watermark>,
    /// Function-scope fixture instances to set up in the child, topo order.
    pub post_fork: Vec<FixtureInstance>,
    /// Fork-fragile resource fixtures to rebuild in the child (`ExecRequest.reinit`).
    pub reinit_in_child: Vec<NodeId>,
    /// Estimated copy-on-write pages the child will touch — seeds the `MemoryGovernor`.
    pub est_cow_pages: u64,
}

impl ForkPlan {
    /// Derive the fork plan from a resolved [`FixturePlan`] and the live snapshot stack (W10).
    ///
    /// `fork_from` is the deepest shared **live** snapshot (`WatermarkStack::deepest_shared`); the
    /// child runs `plan.post_fork` (Function-scope setup, topo order) and rebuilds the fork-fragile
    /// resources gathered from every layer's `reinit_in_child` (W11). `est_cow_pages` seeds the
    /// `MemoryGovernor` from the fork source's resident set (design 05 §6.3); with no snapshot the
    /// child forks the wellspring base and the estimate is unknown (0).
    pub fn from(plan: &FixturePlan, stack: &WatermarkStack) -> Self {
        let fork_from = stack.deepest_shared(plan).cloned();
        let reinit_in_child: Vec<NodeId> = plan
            .layers
            .iter()
            .flat_map(|layer| layer.reinit_in_child.iter().cloned())
            .collect();
        // COW pages the child is *likely* to touch: a conservative fraction of the fork source's
        // RSS (the governor refines this from observed child RSS — W13). 4 KiB pages.
        let est_cow_pages = fork_from
            .as_ref()
            .map(|wm| wm.rss_bytes / 4096)
            .unwrap_or(0);
        Self {
            fork_from,
            post_fork: plan.post_fork.clone(),
            reinit_in_child,
            est_cow_pages,
        }
    }

    /// The fork-from watermark, if any.
    pub fn fork_from(&self) -> Option<&Watermark> {
        self.fork_from.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Scope, ScopePath};
    use crate::exec::watermark::{Watermark, WatermarkId};
    use crate::fixtures::{ClosureHash, FixtureArgs, FixturePlan, ScopeLayer};

    fn plan(layers: Vec<ScopeLayer>, post_fork: Vec<FixtureInstance>) -> FixturePlan {
        FixturePlan::new(
            NodeId::new("m.py::t"),
            layers,
            None,
            post_fork,
            FixtureArgs::new(),
            ClosureHash::from_bytes([0u8; 32]),
        )
    }

    #[test]
    fn from_picks_deepest_shared_and_gathers_reinit() {
        let mut stack = WatermarkStack::new();
        stack.push_layer(Watermark::new(
            WatermarkId::new(1),
            Scope::Module,
            ScopePath::module("m.py"),
            8192,
            7,
        ));
        let mut layer = ScopeLayer::new(Scope::Module, ScopePath::module("m.py"), vec![]);
        layer.reinit_in_child = vec![NodeId::new("m.py::db_conn")];
        let fp = plan(vec![layer], vec![]);

        let plan = ForkPlan::from(&fp, &stack);
        assert_eq!(plan.fork_from().map(|w| w.id()), Some(WatermarkId::new(1)));
        assert_eq!(plan.reinit_in_child, vec![NodeId::new("m.py::db_conn")]);
        assert_eq!(
            plan.est_cow_pages,
            8192 / 4096,
            "estimate seeded from fork-source RSS"
        );
    }

    #[test]
    fn from_with_no_live_snapshot_forks_base() {
        let stack = WatermarkStack::new();
        let fp = plan(
            vec![ScopeLayer::new(
                Scope::Module,
                ScopePath::module("m.py"),
                vec![],
            )],
            vec![],
        );
        let plan = ForkPlan::from(&fp, &stack);
        assert!(
            plan.fork_from().is_none(),
            "no live snapshot → fork the wellspring base"
        );
        assert_eq!(plan.est_cow_pages, 0);
    }
}
