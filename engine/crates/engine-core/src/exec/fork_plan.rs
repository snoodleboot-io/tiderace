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
    /// LANE: Lane WM (wm-fork) implements from — W10.
    pub fn from(_plan: &FixturePlan, _stack: &WatermarkStack) -> Self {
        unimplemented!("LANE: Lane WM (wm-fork) implements ForkPlan::from — W10")
    }

    /// The fork-from watermark, if any.
    pub fn fork_from(&self) -> Option<&Watermark> {
        self.fork_from.as_ref()
    }
}
