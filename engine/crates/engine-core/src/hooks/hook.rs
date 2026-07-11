use crate::hooks::HookEvent;

/// A riptide-native plugin: it observes (and may act on) typed engine [`HookEvent`]s. This is the
/// engine's *own* hook interface (ADR-E001) — not pytest's, no `pluggy`. A future `PyPluginAdapter`
/// will implement `Hook` to bridge a Python plugin in via FFI (rides ②, ADR-E011/E013); native Rust
/// plugins implement it directly for static-dispatch cost.
pub trait Hook {
    /// Handle one lifecycle event. Default no-op so a plugin implements only the events it cares about.
    fn handle(&mut self, event: &HookEvent<'_>) {
        let _ = event;
    }

    /// A stable name for diagnostics / ordering ties.
    fn name(&self) -> &str {
        "unnamed"
    }
}
