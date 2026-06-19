//! `LayeredResolver` (W8) — the Phase 3 [`FixtureResolver`] implementation: walks a test's closure
//! in topo order, bins instances into scope layers, expands parametrized fixtures (W5), chooses
//! `fork_from = deepest live shared layer`, and computes `closure_hash` (design 04 §4.1–4.2).
//!
//! **Implemented by** Lane FX-graph (subagent fx-resolver): W4/W5/W8/W14.
//!
//! ## Algorithm choices (understand-before-applying)
//! - **Layer binning.** Each closure instance is binned into the [`ScopeLayer`] matching its
//!   declared scope; layers are emitted **widest → narrowest** (Session → Package → Module → Class),
//!   never Function — matching the snapshot stack (design 04 §4.2, CONTRACT §2.8). Function-scope
//!   instances (and any `reinit_after_fork` fixture's fragile handle) go to `FixturePlan.post_fork`,
//!   set up in the forked child.
//! - **`fork_from`.** Chosen as the **deepest** (narrowest-scoped) snapshotted layer in the plan —
//!   the narrowest live snapshot the test shares (CONTRACT §4 invariant 2). At plan time no live
//!   `Watermark` exists yet (the wellspring mints it), so we surface the *deepest layer index*
//!   needing a snapshot; the WM lane (`WatermarkStack::deepest_shared`) maps it to a live
//!   `WatermarkId`. We populate `fork_from = None` here (no live snapshot at plan time) and document
//!   the chosen layer via `ScopeLayer.snapshot` placeholders — see [`Self::plan_for`].
//! - **Parametrization (W5).** A parametrized fixture fans out into one `FixtureInstance` per param;
//!   when several are parametrized in one closure the variants are the **cartesian product** (see
//!   [`Self::instances_for`]). Each instance carries its **own** `closure_hash` so variants cache
//!   independently (CONTRACT §4 invariant 4).
//! - **`closure_hash`.** Computed over the post-override, post-parametrization material in topo order
//!   (each fixture's name, scope, dep names, flags; plus the selected param id+index) via
//!   [`crate::fixtures::closure_hash::ClosureHasher`] — stable + deterministic (W14).

use crate::domain::{NodeId, Scope, ScopePath};
use crate::fixtures::closure_hash::{ClosureHash, ClosureHasher};
use crate::fixtures::fixture::Fixture;
use crate::fixtures::fixture_args::FixtureArgs;
use crate::fixtures::fixture_closure::FixtureClosure;
use crate::fixtures::fixture_error::FixtureError;
use crate::fixtures::fixture_graph::FixtureGraph;
use crate::fixtures::fixture_instance::FixtureInstance;
use crate::fixtures::fixture_plan::FixturePlan;
use crate::fixtures::fixture_resolver::FixtureResolver;
use crate::fixtures::override_table::OverrideTable;
use crate::fixtures::param_value::ParamValue;
use crate::fixtures::scope_layer::ScopeLayer;

/// Widest → narrowest scope order for layer emission (Function excluded — it is post-fork).
const LAYER_SCOPES: [Scope; 4] = [Scope::Session, Scope::Package, Scope::Module, Scope::Class];

/// The layered, override-aware resolver.
#[derive(Debug, Default)]
pub struct LayeredResolver {
    /// Override table used to resolve fixture names by location during planning.
    override_table: OverrideTable,
}

impl LayeredResolver {
    /// Construct a resolver over an override table.
    pub fn new(override_table: OverrideTable) -> Self {
        Self { override_table }
    }

    /// The override table this resolver consults.
    pub fn override_table(&self) -> &OverrideTable {
        &self.override_table
    }

    /// Produce **all** whole-test parametrization variants as separate plans (W5 cartesian fan-out
    /// over *tests*).
    ///
    /// While [`Self::plan_for`] returns a single plan whose instance list contains **every** param
    /// variant of each parametrized fixture (the per-fixture fan-out), `plans_for` additionally
    /// produces the cartesian product over parametrized fixtures as *distinct test plans* — the
    /// scheduler multiplies dependent `TestItem`s this way (design 04 §1.3). For `p(params=[a,b])`
    /// and `q(params=[x,y])` it yields 2×2 = 4 plans, each pinning one `(p,q)` selection. An
    /// unparametrized closure yields exactly one plan. Order is deterministic.
    pub fn plans_for(
        &self,
        graph: &FixtureGraph,
        requested: &[String],
        scope_path: &ScopePath,
    ) -> std::result::Result<Vec<FixturePlan>, FixtureError> {
        let closure = self.resolve(graph, requested, scope_path)?;
        let selections = self.parameter_selections(graph, &closure)?;
        let mut plans = Vec::with_capacity(selections.len());
        for selection in selections {
            // Pin each parametrized fixture to its selected param (one variant per axis).
            plans.push(self.build_plan(graph, &closure, scope_path, Some(&selection))?);
        }
        Ok(plans)
    }

    /// The concrete `FixtureInstance`s for a closure — one entry per closure node, fanned out to all
    /// param variants of each parametrized fixture (the per-fixture model used by [`Self::plan_for`]).
    pub fn instances_for(
        &self,
        graph: &FixtureGraph,
        closure: &FixtureClosure,
    ) -> std::result::Result<Vec<FixtureInstance>, FixtureError> {
        self.fan_out_instances(graph, closure, None)
    }

    /// Compute the per-fixture parameter selections forming the cartesian product over all
    /// parametrized fixtures in the closure. Each element is one full assignment: `node → param`
    /// (only parametrized fixtures appear). Returns at least one (possibly empty) selection so an
    /// unparametrized closure produces one plan.
    fn parameter_selections(
        &self,
        graph: &FixtureGraph,
        closure: &FixtureClosure,
    ) -> std::result::Result<Vec<Vec<(NodeId, ParamValue)>>, FixtureError> {
        // Collect parametrized fixtures in topo order with validated param lists.
        let mut axes: Vec<(NodeId, Vec<ParamValue>)> = Vec::new();
        for node in closure.setup_order() {
            let Some(fixture) = graph.fixture(node) else {
                continue;
            };
            if let Some(params) = &fixture.params {
                if params.is_empty() {
                    continue; // an empty param list = unparametrized (single instance).
                }
                Self::validate_params(fixture)?;
                axes.push((node.clone(), params.clone()));
            }
        }

        // Cartesian product. Start with one empty selection; for each axis, expand.
        let mut selections: Vec<Vec<(NodeId, ParamValue)>> = vec![Vec::new()];
        for (node, params) in axes {
            let mut next = Vec::with_capacity(selections.len() * params.len());
            for base in &selections {
                for param in &params {
                    let mut combo = base.clone();
                    combo.push((node.clone(), param.clone()));
                    next.push(combo);
                }
            }
            selections = next;
        }
        Ok(selections)
    }

    /// A parametrized fixture's param list must be non-empty with strictly increasing,
    /// gap-free declaration indices (0..n) so the shim's index-based selection is well-defined.
    fn validate_params(fixture: &Fixture) -> std::result::Result<(), FixtureError> {
        let Some(params) = &fixture.params else {
            return Ok(());
        };
        for (i, p) in params.iter().enumerate() {
            if p.index != i {
                return Err(FixtureError::ParamShapeMismatch {
                    name: fixture.name.clone(),
                });
            }
        }
        Ok(())
    }

    /// Build the ordered `FixtureInstance` list for a closure, fanning out each parametrized fixture
    /// into one instance **per param** (W5). Topo (setup) order is preserved; a parametrized fixture
    /// contributes its variants consecutively in declaration order.
    ///
    /// `pin`: when `Some(selection)`, each parametrized fixture is pinned to its single selected
    /// param (used by `plans_for` for one cartesian variant); when `None`, all params fan out (used
    /// by `plan_for`, so the single plan carries every variant — CONTRACT §4 invariant 4).
    fn fan_out_instances(
        &self,
        graph: &FixtureGraph,
        closure: &FixtureClosure,
        pin: Option<&[(NodeId, ParamValue)]>,
    ) -> std::result::Result<Vec<FixtureInstance>, FixtureError> {
        let mut instances = Vec::with_capacity(closure.setup_order().len());
        for node in closure.setup_order() {
            let Some(fixture) = graph.fixture(node) else {
                return Err(FixtureError::Unresolved {
                    name: node.to_string(),
                    scope_path: fixture_scope_path_or_default(graph, node),
                });
            };
            let params: Vec<Option<ParamValue>> = match &fixture.params {
                Some(p) if !p.is_empty() => {
                    Self::validate_params(fixture)?;
                    match pin.and_then(|sel| sel.iter().find(|(n, _)| n == node)) {
                        // Pinned: just this fixture's selected param.
                        Some((_, chosen)) => vec![Some(chosen.clone())],
                        // Unpinned: every param variant.
                        None => p.iter().map(|pv| Some(pv.clone())).collect(),
                    }
                }
                // Unparametrized: a single instance with no param.
                _ => vec![None],
            };
            for param in params {
                let hash = Self::instance_hash(graph, closure, fixture, param.as_ref());
                instances.push(FixtureInstance::new(node.clone(), param, hash));
            }
        }
        Ok(instances)
    }

    /// Hash one instance's identity over its position in the closure: the fixture's defining
    /// material (name/scope/deps/flags) plus its selected param. Deterministic (W14).
    fn instance_hash(
        graph: &FixtureGraph,
        closure: &FixtureClosure,
        fixture: &Fixture,
        param: Option<&ParamValue>,
    ) -> ClosureHash {
        let mut hasher = ClosureHasher::new();
        // Feed the whole closure's defining material (topo order) so editing *any* transitive
        // fixture body's identity invalidates dependents (design 04 §8), then the instance's own
        // selected param so each variant differs.
        for node in closure.setup_order() {
            if let Some(f) = graph.fixture(node) {
                Self::feed_fixture(&mut hasher, f);
            }
        }
        hasher.feed_str("@instance");
        hasher.feed_str(&fixture.name);
        match param {
            Some(p) => {
                hasher.feed_str(p.id());
                hasher.feed_u64(p.index() as u64);
            }
            None => {
                hasher.feed_str("@noparam");
            }
        }
        hasher.finish()
    }

    /// Feed one fixture's stable defining material into the hasher.
    fn feed_fixture(hasher: &mut ClosureHasher, f: &Fixture) {
        hasher.feed_str(&f.name);
        hasher.feed_u64(f.scope.rank() as u64);
        hasher.feed_str(&f.scope_path.module);
        if let Some(class) = &f.scope_path.class {
            hasher.feed_str(class);
        }
        for dep in &f.deps {
            hasher.feed_str(dep);
        }
        hasher.feed_u64(f.autouse as u64);
        hasher.feed_u64(f.is_yield as u64);
        hasher.feed_u64(f.reinit_after_fork as u64);
    }

    /// Build a full plan from a closure. `pin = None` (the [`Self::plan_for`] case) fans every
    /// parametrized fixture out to all variants within the one plan; `pin = Some(selection)` (the
    /// [`Self::plans_for`] case) pins each parametrized fixture to one selected param.
    fn build_plan(
        &self,
        graph: &FixtureGraph,
        closure: &FixtureClosure,
        scope_path: &ScopePath,
        pin: Option<&[(NodeId, ParamValue)]>,
    ) -> std::result::Result<FixturePlan, FixtureError> {
        let instances = self.fan_out_instances(graph, closure, pin)?;
        let test_node = NodeId::new(scope_path.module.clone());

        // Bin instances into widest→narrowest layers; Function + reinit handles → post_fork.
        let mut layers: Vec<ScopeLayer> = Vec::new();
        let mut post_fork: Vec<FixtureInstance> = Vec::new();
        let mut args = FixtureArgs::new();

        for scope in LAYER_SCOPES {
            let mut setup: Vec<FixtureInstance> = Vec::new();
            let mut reinit: Vec<NodeId> = Vec::new();
            for inst in &instances {
                let Some(fixture) = graph.fixture(inst.fixture()) else {
                    continue;
                };
                if fixture.scope != scope {
                    continue;
                }
                setup.push(inst.clone());
                if fixture.reinit_after_fork {
                    // Pure part stays at this layer; fragile handle is rebuilt per child.
                    reinit.push(inst.fixture().clone());
                    post_fork.push(inst.clone());
                }
                args.bind(fixture.name.clone(), inst.clone());
            }
            if !setup.is_empty() {
                let mut layer = ScopeLayer::new(scope, scope_path.clone(), setup);
                layer.reinit_in_child = reinit;
                layers.push(layer);
            }
        }

        // Function-scope instances always run post-fork (in declared/topo order).
        for inst in &instances {
            if let Some(fixture) = graph.fixture(inst.fixture()) {
                if fixture.scope == Scope::Function {
                    post_fork.push(inst.clone());
                    args.bind(fixture.name.clone(), inst.clone());
                }
            }
        }

        // `fork_from`: the deepest (narrowest) shared **live** snapshot. The resolver only *plans* —
        // at plan time no live `Watermark` has been minted (the wellspring mints it when it
        // materializes a layer, CONTRACT §2.7/§2.8). So we emit `fork_from = None` (and every
        // `ScopeLayer.snapshot = None`); the WM lane's `WatermarkStack::deepest_shared(plan)` reads
        // the widest→narrowest layer order and maps the deepest live layer to a `WatermarkId`. The
        // layer ordering *is* the fork-source signal — the last (narrowest, wider-than-Function)
        // non-empty layer is the canonical fork source.
        let fork_from = None;

        // Plan-level closure hash: over all instances' hashes in order (each already per-variant).
        let mut plan_hasher = ClosureHasher::new();
        plan_hasher.feed_str(test_node.as_str());
        for inst in &instances {
            plan_hasher.feed(inst.closure_hash().as_bytes());
        }
        let closure_hash = plan_hasher.finish();

        Ok(FixturePlan::new(
            test_node,
            layers,
            fork_from,
            post_fork,
            args,
            closure_hash,
        ))
    }
}

/// Best-effort scope path for an error message when a node is missing.
fn fixture_scope_path_or_default(graph: &FixtureGraph, node: &NodeId) -> ScopePath {
    graph
        .fixture(node)
        .map(|f| f.scope_path.clone())
        .unwrap_or_else(|| ScopePath::module(""))
}

impl FixtureResolver for LayeredResolver {
    fn resolve(
        &self,
        graph: &FixtureGraph,
        requested: &[String],
        scope_path: &ScopePath,
    ) -> std::result::Result<FixtureClosure, FixtureError> {
        graph.closure_of(requested, scope_path)
    }

    fn layer_assignment(
        &self,
        graph: &FixtureGraph,
        closure: &FixtureClosure,
        scope_path: &ScopePath,
    ) -> std::result::Result<Vec<ScopeLayer>, FixtureError> {
        // Layers from the per-fixture fan-out (all param variants in one plan).
        let plan = self.build_plan(graph, closure, scope_path, None)?;
        Ok(plan.layers)
    }

    fn plan_for(
        &self,
        graph: &FixtureGraph,
        requested: &[String],
        scope_path: &ScopePath,
    ) -> std::result::Result<FixturePlan, FixtureError> {
        let closure = self.resolve(graph, requested, scope_path)?;
        // A single plan whose instance list carries every parametrization variant (per-fixture
        // fan-out). The cartesian product over *tests* is exposed separately via `plans_for`.
        self.build_plan(graph, &closure, scope_path, None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::param_value::ParamValue;

    fn fix(id: &str, name: &str, scope: Scope) -> Fixture {
        Fixture::new(NodeId::new(id), name, scope, ScopePath::module(""))
    }

    fn table(fixtures: &[Fixture]) -> OverrideTable {
        let mut t = OverrideTable::new();
        for f in fixtures {
            t.insert(f.name.clone(), &f.scope_path, f.node_id.clone());
        }
        t
    }

    fn graph_of(fixtures: Vec<Fixture>) -> (FixtureGraph, LayeredResolver) {
        let t = table(&fixtures);
        let g = FixtureGraph::build(fixtures, &t).expect("builds");
        (g, LayeredResolver::new(OverrideTable::new()))
    }

    #[test]
    fn resolve_delegates_to_closure() {
        // happy.
        let db = fix("db", "db", Scope::Session);
        let (g, r) = graph_of(vec![db]);
        let c = r
            .resolve(&g, &["db".into()], &ScopePath::module("m.py"))
            .expect("resolve");
        assert_eq!(c.setup_order().len(), 1);
    }

    #[test]
    fn layers_widest_to_narrowest_and_function_post_fork() {
        // ordering: session/module land in layers (S before M); function in post_fork.
        let db = fix("db", "db", Scope::Session);
        let seeded = fix("seeded", "seeded", Scope::Module).with_deps(vec!["db".into()]);
        let order = fix("order", "order", Scope::Function).with_deps(vec!["seeded".into()]);
        let (g, r) = graph_of(vec![db, seeded, order]);

        let plan = r
            .plan_for(&g, &["order".into()], &ScopePath::module("test_orders.py"))
            .expect("plan");

        let scopes: Vec<Scope> = plan.layers.iter().map(|l| l.scope).collect();
        assert_eq!(scopes, vec![Scope::Session, Scope::Module]);
        assert!(plan.layers.iter().all(|l| l.scope != Scope::Function));
        // function fixture is post-fork.
        assert!(plan
            .post_fork
            .iter()
            .any(|i| i.fixture().as_str() == "order"));
        // fixture_args binds all three by name.
        assert!(plan.fixture_args.get("db").is_some());
        assert!(plan.fixture_args.get("seeded").is_some());
        assert!(plan.fixture_args.get("order").is_some());
    }

    #[test]
    fn empty_closure_yields_empty_plan() {
        // empty.
        let (g, r) = graph_of(vec![]);
        let plan = r
            .plan_for(&g, &[], &ScopePath::module("m.py"))
            .expect("plan");
        assert!(plan.layers.is_empty());
        assert!(plan.post_fork.is_empty());
        assert!(plan.fixture_args.is_empty());
    }

    #[test]
    fn reinit_after_fork_in_layer_and_post_fork() {
        // boundary / W11 encoding: session fixture with reinit_after_fork appears in the layer's
        // reinit_in_child AND in post_fork.
        let db = fix("db", "db", Scope::Session).reinit_after_fork();
        let (g, r) = graph_of(vec![db]);
        let plan = r
            .plan_for(&g, &["db".into()], &ScopePath::module("m.py"))
            .expect("plan");
        let session_layer = plan
            .layers
            .iter()
            .find(|l| l.scope == Scope::Session)
            .expect("session layer");
        assert_eq!(session_layer.reinit_in_child, vec![NodeId::new("db")]);
        assert!(plan.post_fork.iter().any(|i| i.fixture().as_str() == "db"));
    }

    #[test]
    fn parametrized_fans_out_cartesian() {
        // W5: p=[a,b] × q=[x,y] = 4 plans, all distinct closure_hash.
        let p = fix("p", "p", Scope::Function)
            .with_params(vec![ParamValue::new("a", 0), ParamValue::new("b", 1)]);
        let q = fix("q", "q", Scope::Function)
            .with_params(vec![ParamValue::new("x", 0), ParamValue::new("y", 1)]);
        let t = fix("t", "t", Scope::Function).with_deps(vec!["p".into(), "q".into()]);
        let (g, r) = graph_of(vec![p, q, t]);

        let plans = r
            .plans_for(&g, &["t".into()], &ScopePath::module("m.py"))
            .expect("plans");
        assert_eq!(plans.len(), 4, "cartesian product 2x2");
        let hashes: std::collections::HashSet<_> =
            plans.iter().map(|pl| pl.closure_hash()).collect();
        assert_eq!(hashes.len(), 4, "each variant a distinct closure_hash");
    }

    #[test]
    fn distinct_closure_hash_per_param_instance() {
        // W14: same fixture, different param ⇒ distinct instance closure_hash.
        let p = fix("p", "p", Scope::Function)
            .with_params(vec![ParamValue::new("a", 0), ParamValue::new("b", 1)]);
        let (g, r) = graph_of(vec![p]);
        let closure = r
            .resolve(&g, &["p".into()], &ScopePath::module("m.py"))
            .expect("closure");
        let instances = r.instances_for(&g, &closure).expect("instances");
        // One closure node `p`, fanned out into 2 param variants.
        let variants: Vec<_> = instances.iter().filter(|i| i.param().is_some()).collect();
        assert_eq!(variants.len(), 2);
        assert_ne!(
            variants[0].closure_hash(),
            variants[1].closure_hash(),
            "param variants hash differently"
        );
    }

    #[test]
    fn param_shape_mismatch_errors() {
        // error: param indices not 0..n.
        let bad = fix("bad", "bad", Scope::Function)
            .with_params(vec![ParamValue::new("a", 0), ParamValue::new("b", 5)]);
        let (g, r) = graph_of(vec![bad]);
        let err = r
            .plan_for(&g, &["bad".into()], &ScopePath::module("m.py"))
            .unwrap_err();
        assert!(matches!(err, FixtureError::ParamShapeMismatch { name } if name == "bad"));
    }

    #[test]
    fn closure_hash_stable_across_calls() {
        // determinism: identical inputs ⇒ identical plan closure_hash.
        let db = fix("db", "db", Scope::Session);
        let (g, r) = graph_of(vec![db]);
        let a = r
            .plan_for(&g, &["db".into()], &ScopePath::module("m.py"))
            .expect("a");
        let b = r
            .plan_for(&g, &["db".into()], &ScopePath::module("m.py"))
            .expect("b");
        assert_eq!(a.closure_hash(), b.closure_hash());
    }

    #[test]
    fn editing_transitive_fixture_changes_hash() {
        // W14 precision: changing a transitive dep's identity changes the dependent's hash.
        let db1 = fix("db", "db", Scope::Session);
        let order1 = fix("order", "order", Scope::Function).with_deps(vec!["db".into()]);
        let (g1, r1) = graph_of(vec![db1, order1]);
        let h1 = r1
            .plan_for(&g1, &["order".into()], &ScopePath::module("m.py"))
            .expect("h1")
            .closure_hash();

        // db now yields (different identity material).
        let db2 = fix("db", "db", Scope::Session).yielding();
        let order2 = fix("order", "order", Scope::Function).with_deps(vec!["db".into()]);
        let (g2, r2) = graph_of(vec![db2, order2]);
        let h2 = r2
            .plan_for(&g2, &["order".into()], &ScopePath::module("m.py"))
            .expect("h2")
            .closure_hash();

        assert_ne!(h1, h2, "editing transitive db invalidates order's hash");
    }
}
