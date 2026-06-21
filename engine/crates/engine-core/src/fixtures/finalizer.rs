//! `Finalizer` (W7) — the teardown half of a yield-style fixture, captured per active instance.
//!
//! Finalizers run in **strict reverse setup order** at the moment their owning scope tears down.
//! Snapshotted-scope (Session/Module/Class) finalizers run **once** when the layer retires; only
//! Function-scope finalizers run inside each forked child (design 04 §1.1, §4). Rust owns the
//! ordering; the shim owns invoking the continuation — so this is pure data (the *invocation* is the
//! worker/shim's job, not a method on this type), fully defined here.

use serde::{Deserialize, Serialize};

use crate::domain::Scope;
use crate::fixtures::fixture_instance::FixtureInstance;
use crate::fixtures::shim_handle::ShimHandle;

/// A captured teardown continuation bound to the instance + scope it belongs to.
///
/// The owning `ScopeLayer` (for snapshotted scopes) or the child run (for Function scope) holds an
/// ordered list of these; teardown replays them in reverse capture order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finalizer {
    /// The instance whose teardown this is.
    pub instance: FixtureInstance,
    /// The scope at whose exit this finalizer runs (== the instance's fixture scope).
    pub scope: Scope,
    /// The shim-side continuation to invoke.
    pub continuation: ShimHandle,
}

impl Finalizer {
    /// Capture a finalizer for `instance` at `scope`, referencing the shim continuation `continuation`.
    pub fn new(instance: FixtureInstance, scope: Scope, continuation: ShimHandle) -> Self {
        Self {
            instance,
            scope,
            continuation,
        }
    }

    /// The scope at whose exit this finalizer runs.
    pub fn scope(&self) -> Scope {
        self.scope
    }

    /// The shim continuation token to invoke at teardown.
    pub fn continuation(&self) -> ShimHandle {
        self.continuation
    }

    /// `true` if this finalizer belongs to a snapshotted scope (Session/Package/Module/Class) — it
    /// runs **once** when its layer retires, not per forked child (design 04 §1.1, CONTRACT §4
    /// invariant 6). Function-scope finalizers (the complement) run per child, in-child.
    pub fn is_snapshotted_scope(&self) -> bool {
        self.scope != Scope::Function
    }

    /// Teardown order for a list of captured finalizers: the **strict reverse** of capture (setup)
    /// order (W7, design 04 §1.1). Rust owns this ordering; the shim invokes each continuation.
    ///
    /// The load-bearing W7 contribution from Lane FX-graph: given the order a scope's finalizers were
    /// captured in (== fixture setup order), teardown is its exact reverse. Snapshotted-scope
    /// finalizers are replayed once at layer retire; Function finalizers once per child — both in
    /// this reverse order within their group.
    pub fn teardown_order(captured: &[Finalizer]) -> Vec<Finalizer> {
        captured.iter().rev().cloned().collect()
    }

    /// Partition captured finalizers into `(snapshotted_scope, function_scope)` groups, **preserving**
    /// capture order within each group. Callers tear down each group via [`Self::teardown_order`]:
    /// function finalizers per child, snapshotted-scope finalizers once at layer retire (CONTRACT §4
    /// invariant 6).
    pub fn partition_by_runcount(captured: &[Finalizer]) -> (Vec<Finalizer>, Vec<Finalizer>) {
        let mut snapshotted = Vec::new();
        let mut function = Vec::new();
        for f in captured {
            if f.is_snapshotted_scope() {
                snapshotted.push(f.clone());
            } else {
                function.push(f.clone());
            }
        }
        (snapshotted, function)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::NodeId;
    use crate::fixtures::closure_hash::ClosureHash;
    use crate::fixtures::fixture_instance::FixtureInstance;

    fn fin(name: &str, scope: Scope, handle: u64) -> Finalizer {
        let inst =
            FixtureInstance::new(NodeId::new(name), None, ClosureHash::from_bytes([0u8; 32]));
        Finalizer::new(inst, scope, ShimHandle::new(handle))
    }

    #[test]
    fn teardown_is_strict_reverse_of_setup() {
        // ordering: captured a,b,c → teardown c,b,a.
        let captured = vec![
            fin("a", Scope::Module, 1),
            fin("b", Scope::Module, 2),
            fin("c", Scope::Module, 3),
        ];
        let order: Vec<u64> = Finalizer::teardown_order(&captured)
            .iter()
            .map(|f| f.continuation().get())
            .collect();
        assert_eq!(order, vec![3, 2, 1]);
    }

    #[test]
    fn teardown_empty_is_empty() {
        // empty.
        assert!(Finalizer::teardown_order(&[]).is_empty());
    }

    #[test]
    fn teardown_single() {
        // boundary.
        let captured = vec![fin("a", Scope::Function, 7)];
        let order = Finalizer::teardown_order(&captured);
        assert_eq!(order.len(), 1);
        assert_eq!(order[0].continuation().get(), 7);
    }

    #[test]
    fn snapshotted_vs_function_run_count_partition() {
        // run-count semantics: snapshotted scopes vs function scope are partitioned distinctly.
        let captured = vec![
            fin("session", Scope::Session, 1),
            fin("module", Scope::Module, 2),
            fin("func", Scope::Function, 3),
        ];
        let (snap, func) = Finalizer::partition_by_runcount(&captured);
        assert_eq!(snap.len(), 2, "session+module run once at layer retire");
        assert_eq!(func.len(), 1, "function runs per child");
        assert!(!func[0].is_snapshotted_scope());
    }

    #[test]
    fn is_snapshotted_scope_classification() {
        // authz-n/a substitute: classify each scope correctly.
        assert!(fin("s", Scope::Session, 0).is_snapshotted_scope());
        assert!(fin("p", Scope::Package, 0).is_snapshotted_scope());
        assert!(fin("m", Scope::Module, 0).is_snapshotted_scope());
        assert!(fin("c", Scope::Class, 0).is_snapshotted_scope());
        assert!(!fin("f", Scope::Function, 0).is_snapshotted_scope());
    }

    #[test]
    fn error_during_teardown_remaining_still_ordered() {
        // adversarial: the ORDER is produced wholesale; even if the shim errors mid-way, the
        // remaining finalizers are already enumerated (Rust owns ordering, shim owns invocation).
        // Here we assert the full reverse list is materialized up-front so a mid-teardown error
        // cannot drop later finalizers from the plan.
        let captured = vec![
            fin("a", Scope::Function, 10),
            fin("b", Scope::Function, 20),
            fin("c", Scope::Function, 30),
        ];
        let order = Finalizer::teardown_order(&captured);
        // Simulate the shim failing on the first (handle 30); the rest are still present.
        let remaining: Vec<u64> = order[1..].iter().map(|f| f.continuation().get()).collect();
        assert_eq!(remaining, vec![20, 10]);
    }
}
