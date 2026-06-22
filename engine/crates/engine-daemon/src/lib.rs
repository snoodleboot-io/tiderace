//! `engine-daemon` — the warm test server (Phase 6, design 08, ADR-E007).
//!
//! A long-lived, per-project host that keeps the expensive things warm between invocations —
//! imported Python (the wellspring), the result cache, and collection/dependency state — so an
//! edit→result inner loop can hit sub-100ms. A thin CLI/IDE talks to it over a local socket
//! ([`RpcRequest`]/[`RpcResponse`]); on each file change the [`Session`] composes content-hash
//! [`Invalidator`] → impact selection → cache filtering into the minimum re-run ([`ChangeOutcome`]).
//!
//! This crate currently provides the daemon's testable **brain** (protocol, invalidation, the
//! incremental session, FS-watch coalescing). The socket/process lifecycle glue layers on top of
//! these pieces. One type per file (ADR-E005), mirroring design 08.

mod fs_watcher;
mod invalidator;
mod rpc_method;
mod session;

pub use fs_watcher::{Debouncer, FsWatcher};
pub use invalidator::{Invalidation, Invalidator};
pub use rpc_method::{RpcRequest, RpcResponse, RpcResult};
pub use session::{ChangeOutcome, Session};
