//! `FixtureArgs` — the assembled argument map a test body is invoked with.
//!
//! Maps each requested fixture **parameter name** (the argument the test/​fixture body declares) to
//! the [`FixtureInstance`] that satisfies it after override + parametrization resolution. The shim
//! uses it to bind the body's parameters to fixture values in-child. Carried on `ExecRequest`
//! (design 05 §5.2). Pure data — fully defined.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::fixtures::fixture_instance::FixtureInstance;

/// The resolved argument-name → fixture-instance binding for one test (or fixture) body.
///
/// `BTreeMap` keeps a deterministic order — important because this map participates in the
/// reproducible material the shim consumes and (indirectly) the closure hash reflects.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FixtureArgs {
    /// Argument name (as declared by the consuming body) → satisfying instance.
    pub bindings: BTreeMap<String, FixtureInstance>,
}

impl FixtureArgs {
    /// An empty argument map (a body requesting no fixtures).
    pub fn new() -> Self {
        Self::default()
    }

    /// Bind argument `name` to `instance`, returning the previous binding if one existed.
    pub fn bind(
        &mut self,
        name: impl Into<String>,
        instance: FixtureInstance,
    ) -> Option<FixtureInstance> {
        self.bindings.insert(name.into(), instance)
    }

    /// The instance bound to `name`, if any.
    pub fn get(&self, name: &str) -> Option<&FixtureInstance> {
        self.bindings.get(name)
    }

    /// `true` if no fixtures are bound.
    pub fn is_empty(&self) -> bool {
        self.bindings.is_empty()
    }
}
