//! Purity / impurity policy for cache soundness (ADR-E004, staged).
//!
//! A test outcome may only be cached if it is a **pure function of its inputs**. Impure tests (clock,
//! network, RNG, unrecorded filesystem) must never be silently cached. ADR-E004 stages the detection:
//! the executed-source closure already comes from coverage (E006); sandboxed fs/env/net/clock
//! interception is the heavier follow-on. This module is the **policy seam** both feed into — the
//! orchestrator gates [`Cache::put`](crate::cache::Cache::put) on [`Purity::is_cacheable`].

/// Whether a test's outcome may be cached, and why not when it may not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Purity {
    /// Pure within the observed closure — safe to cache.
    Pure,
    /// Impure — must run every time. The reason is surfaced in diagnostics (`--why-uncacheable`).
    Impure(String),
}

impl Purity {
    /// Mark impure with a human reason (e.g. "read wall clock", "opened a socket").
    pub fn impure(reason: impl Into<String>) -> Self {
        Self::Impure(reason.into())
    }

    /// Whether an outcome with this verdict may be stored.
    pub fn is_cacheable(&self) -> bool {
        matches!(self, Purity::Pure)
    }

    /// The impurity reason, if any.
    pub fn reason(&self) -> Option<&str> {
        match self {
            Purity::Pure => None,
            Purity::Impure(r) => Some(r),
        }
    }
}

/// Observes a test's side effects to decide [`Purity`] (ADR-E004 soundness, staged). The default
/// [`NoSandbox`] makes no observations and trusts the coverage-derived closure (conservative because
/// the closure already reflects what the test *actually* touched); a future sandboxing collector
/// (fs/env/net/clock interception) implements this trait to *detect* impurity and pin or exclude it.
pub trait SandboxHooks: Send + Sync {
    /// The purity verdict after a test ran. `node_id` identifies the test for diagnostics.
    fn verdict(&self, node_id: &str) -> Purity;
}

/// The no-op sandbox: every test is treated as pure (relies on the coverage closure for soundness).
#[derive(Debug, Default, Clone, Copy)]
pub struct NoSandbox;

impl SandboxHooks for NoSandbox {
    fn verdict(&self, _node_id: &str) -> Purity {
        Purity::Pure
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pure_is_cacheable_impure_is_not() {
        assert!(Purity::Pure.is_cacheable());
        let imp = Purity::impure("read wall clock");
        assert!(!imp.is_cacheable());
        assert_eq!(imp.reason(), Some("read wall clock"));
    }

    #[test]
    fn no_sandbox_treats_all_pure() {
        assert_eq!(NoSandbox.verdict("t.py::a"), Purity::Pure);
    }
}
