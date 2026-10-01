//! How a shim's answer — or the lack of one — becomes [`TestResult`]s (TID-114). Four places
//! built an `Outcome::Error` result by hand with a format string for a detail; this names
//! each reason, and the one mapping from a response to results lives here.

use std::time::Duration;

use crate::domain::{NodeId, Outcome, TestItem, TestResult};
use crate::exec::shim_protocol::ExecResponse;

/// Why a test has no result of its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotRun {
    /// The worker stopped answering on this very test: `fault` is what the transport said.
    WorkerFault { fault: String },
    /// A test earlier in the batch took the worker down; this one never started.
    AfterLostWorker { at: NodeId, fault: String },
    /// The sub-interpreter pool answered the batch, but not for this node.
    NoReply,
    /// The sub-interpreter pool gave no answer within the batch's budget and was killed (TID-104).
    PoolKilled { budget: Duration },
}

impl std::fmt::Display for NotRun {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NotRun::WorkerFault { fault } => f.write_str(fault),
            NotRun::AfterLostWorker { at, fault } => {
                write!(f, "not run: the worker was lost at {at} ({fault})")
            }
            NotRun::NoReply => f.write_str("no result returned by subinterp pool"),
            NotRun::PoolKilled { budget } => write!(
                f,
                "no result from the sub-interpreter pool within {:.0}s — a test in this \
                 batch blocked where nothing could interrupt it; the pool was killed (TID-104)",
                budget.as_secs_f64()
            ),
        }
    }
}

impl TestResult {
    /// An `Error` result for a test that produced none, saying why.
    pub fn not_run(node_id: NodeId, why: NotRun, elapsed: Duration) -> Self {
        TestResult::new(
            node_id,
            Outcome::Error,
            elapsed.as_millis() as u64,
            why.to_string(),
        )
    }
}

impl ExecResponse {
    /// The results this response stands for: the node's own, or one per case when the node
    /// expanded (TID-25) — and none at all for an empty expansion, which is how a deselected node
    /// and a class that inherits nothing report themselves. Every tier reads a response through
    /// this (TID-104): one that read it as a single outcome counted a deselected node as a pass.
    pub fn into_results(self, item: &TestItem, elapsed: Duration) -> Vec<TestResult> {
        // A parametrized node reports one result per case (TID-25). The cases already ran and
        // forked individually, so this reports what was executed rather than the worst of it.
        if self.expanded || !self.variants.is_empty() {
            return self
                .variants
                .into_iter()
                .map(|v| {
                    let touched = v.coverage.keys().cloned().collect();
                    TestResult::new(v.node_id, v.outcome, v.duration_ms, v.detail)
                        .with_touched(touched)
                        .with_pure(v.pure)
                        .with_must_fork(v.must_fork)
                        .with_keywords(v.keywords)
                        // These ids did not come from the static collector — they were produced
                        // here, by expanding a parametrized node or an inherited class (TID-55).
                        .with_expanded(true)
                })
                .collect();
        }
        let touched = self.coverage.keys().cloned().collect();
        vec![TestResult::new(
            item.node_id.clone(),
            self.outcome,
            elapsed.as_millis() as u64,
            self.detail,
        )
        .with_touched(touched)
        .with_pure(self.pure)
        .with_must_fork(self.must_fork)
        .with_skip_origin(self.skip_origin)
        .with_keywords(self.keywords)]
    }
}
