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

mod collection;
mod config;
mod engine_handler;
mod error;
mod fs_watcher;
mod full_run;
mod impacted_run;
mod invalidator;
mod result_cache;
mod rpc;
mod session;
mod state;
mod tree_stamp;
mod warm_image;
mod watch;

pub use config::DaemonConfig;
pub(crate) use engine_handler::to_rpc;
pub use engine_handler::{EngineHandler, ImpactSummary};
pub use error::DaemonError;
pub use fs_watcher::{Debouncer, FsWatcher};
pub use invalidator::{Invalidation, Invalidator};
// Moved to `engine-core` (TID-17) so the CLI can reach the sub-interpreter tier too;
// re-exported here to keep the daemon's public surface unchanged.
pub use engine_core::exec::probe_modules;
pub use rpc::client::{DaemonClient, Healthy, Started};
pub use rpc::method::{RpcRequest, RpcResponse, RpcResult};
pub use rpc::server::{read_frame, serve_connection, write_frame, RpcHandler};
pub use rpc::socket::daemon_socket_path;
#[cfg(unix)]
pub use rpc::socket::serve_unix_socket;
pub use session::{ChangeOutcome, Session};
pub use state::plan::{changed_files, plan, PersistedState, Plan, TestRecord};
pub use watch::{content_hash, react_to_change, watch_loop, WatchAction};
