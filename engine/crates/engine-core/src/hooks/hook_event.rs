use crate::domain::{NodeId, RunReport, TestResult};

/// A typed engine lifecycle event dispatched to registered [`Hook`](crate::hooks::Hook)s (design 12).
/// Borrows its payload — dispatch is a cheap `&HookEvent` hand-off, not a Python call chain (the
/// `pluggy` tax ADR-E001 rejects). Events fire in the **orchestrator/daemon** process, not per-fork.
#[derive(Debug)]
pub enum HookEvent<'a> {
    /// The run is starting (before collection/execution).
    SessionStart,
    /// Collection finished; `count` tests were collected.
    CollectionDone { count: usize },
    /// A test is about to run.
    TestStart(&'a NodeId),
    /// A test finished with this result.
    TestFinish(&'a TestResult),
    /// The run finished; the aggregate report is available.
    SessionFinish(&'a RunReport),
}
