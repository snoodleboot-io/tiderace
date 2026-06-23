//! Plugin / hook host (Phase 7, design 12, ADR-E001) — riptide owns the hooks; **no `pluggy`**.
//!
//! A Rust-native [`HookHost`] dispatches typed lifecycle [`HookEvent`]s to registered [`Hook`]
//! plugins by static method call over a `Vec` — none of pytest's per-call, Python-level dispatch
//! tax. Ordering is a [`Priority`] integer + stable registration order, resolved **once**.
//!
//! Native plugins implement [`Hook`] directly. A `PyPluginAdapter` (the staged pytest-plugin
//! compat boundary, design 12) is deferred: it implements `Hook` to bridge a Python plugin in via
//! FFI, so it lands with the ② in-process backend (ADR-E011/E013), not here. One type per file.

mod hook;
mod hook_event;
mod hook_host;
mod priority;

pub use hook::Hook;
pub use hook_event::HookEvent;
pub use hook_host::HookHost;
pub use priority::Priority;
