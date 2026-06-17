//! Execution substrate — a warm Wellspring forks a pristine child per test over a length-prefixed
//! binary IPC shim (ADR-E002/E003). No pytest underneath.
//!
//! Phase 2 ships the default [`ForkWorker`]; [`Worker`] is the seam behind which the no-fork
//! `SubprocessWorker` (ADR-E008), free-threaded, and remote variants land in later phases.

mod fork_worker;
mod shim_protocol;
mod wellspring;
mod worker;

pub use fork_worker::ForkWorker;
pub use shim_protocol::{read_frame, write_frame, ExecRequest, ExecResponse};
pub use wellspring::Wellspring;
pub use worker::Worker;
