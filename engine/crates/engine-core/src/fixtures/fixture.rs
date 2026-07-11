//! The `Fixture` model (W1) — the user-facing dependency-injection contract and the spine of the
//! fixture subsystem. Pure data: fully defined here (no scaffold).

use serde::{Deserialize, Serialize};

use crate::domain::{NodeId, Scope, ScopePath};
use crate::fixtures::param_value::ParamValue;

/// A named, scoped provider that a `TestItem` — or another fixture — may request by name. Mirrors
/// the subset of the pytest fixture contract that matters for adoption (design 04 §1).
///
/// `deps` are stored as **names** (`Vec<String>`); name → [`NodeId`] resolution happens in the
/// `FixtureGraph` via nearest-override rules (W2/W6), because the same name resolves to different
/// definitions depending on the requesting test's `scope_path`. `node_id` is this definition's own
/// id once it is interned into the graph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fixture {
    /// This fixture definition's node id (assigned when interned into the graph).
    pub node_id: NodeId,
    /// How a test/fixture requests it.
    pub name: String,
    /// Lifetime / how often it is set up.
    pub scope: Scope,
    /// Other fixtures this one requests, **by name** (resolved to node ids in the graph).
    pub deps: Vec<String>,
    /// Injected into every in-scope test's closure without being requested by name.
    pub autouse: bool,
    /// Parameter set for a parametrized fixture; `None` (or empty) ⇒ a single, unparametrized
    /// instance. A non-empty set fans out into N [`crate::fixtures::FixtureInstance`]s (W5).
    pub params: Option<Vec<ParamValue>>,
    /// `true` if the body `yield`s a value then runs teardown code (captured as a `Finalizer`);
    /// `false` if it merely `return`s a value.
    pub is_yield: bool,
    /// `true` if the body acquires a fork-fragile resource (socket, fd, GPU/DB handle, thread pool).
    /// Its pure part may still be snapshotted at the declared `scope`; the fragile handle is rebuilt
    /// per child via `reinit_in_child` / `ExecRequest.reinit` (W11, design 04 §4.3).
    pub reinit_after_fork: bool,
    /// Where this definition lives — the key (with `name`) into the override table for
    /// nearest/longest-prefix resolution (W6, design 04 §1.4).
    pub scope_path: ScopePath,
}

impl Fixture {
    /// Construct a plain (unparametrized, return-style, fork-safe, non-autouse) fixture. Builder-style
    /// setters below flip the optional traits; keeps call sites readable without a wide constructor.
    pub fn new(
        node_id: NodeId,
        name: impl Into<String>,
        scope: Scope,
        scope_path: ScopePath,
    ) -> Self {
        Self {
            node_id,
            name: name.into(),
            scope,
            deps: Vec::new(),
            autouse: false,
            params: None,
            is_yield: false,
            reinit_after_fork: false,
            scope_path,
        }
    }

    /// Set the dependency names this fixture requests.
    pub fn with_deps(mut self, deps: Vec<String>) -> Self {
        self.deps = deps;
        self
    }

    /// Mark this fixture `autouse`.
    pub fn autouse(mut self) -> Self {
        self.autouse = true;
        self
    }

    /// Attach a parameter set (fans out into N instances at resolution).
    pub fn with_params(mut self, params: Vec<ParamValue>) -> Self {
        self.params = Some(params);
        self
    }

    /// Mark this fixture as yield-style (has a teardown half ⇒ a `Finalizer`).
    pub fn yielding(mut self) -> Self {
        self.is_yield = true;
        self
    }

    /// Mark this fixture as acquiring a fork-fragile resource (split-setup, W11).
    pub fn reinit_after_fork(mut self) -> Self {
        self.reinit_after_fork = true;
        self
    }

    /// This definition's node id.
    pub fn node_id(&self) -> &NodeId {
        &self.node_id
    }

    /// The fixture's declared scope.
    pub fn scope(&self) -> Scope {
        self.scope
    }

    /// `true` if this fixture declares a parameter set with at least one entry.
    pub fn is_parametrized(&self) -> bool {
        self.params.as_ref().is_some_and(|p| !p.is_empty())
    }
}
