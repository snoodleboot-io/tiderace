//! Execution substrate — a warm Wellspring forks a pristine child per test over a length-prefixed
//! binary IPC shim (ADR-E002/E003). No pytest underneath.
//!
//! Phase 2 ships the default [`ForkWorker`]; [`Worker`] is the seam behind which the no-fork
//! [`SubprocessWorker`] (ADR-E008), free-threaded, and remote variants land.
//!
//! Phase 3 adds the snapshot-layer machinery the fixture graph drives: [`Watermark`] /
//! [`WatermarkStack`] (the session→module→class layer stack), [`ForkPlan`] (fork-from-deepest),
//! [`MemoryGovernor`] / [`ForkPermit`] (RSS-bounded concurrency), [`SubprocessWorker`] (no-COW
//! fallback), and [`WorkerCaps`]. See the Phase 3 `CONTRACT.md`.

mod fork_permit;
mod fork_plan;
mod memory_governor;
mod process;
mod safe_set_cache;
mod selection;
mod shim_protocol;
mod tiers;
mod transport;
mod watermark;
mod watermark_stack;
mod worker;
mod worker_caps;

pub use fork_permit::ForkPermit;
pub use fork_plan::ForkPlan;
pub use memory_governor::MemoryGovernor;
pub use process::{ShimLaunch, ShimMode, ShimProcess, ShimTarget};
pub use safe_set_cache::{SafeModule, SafeSetCache};
pub use selection::{KeywordExpr, Selection};
pub use shim_protocol::{read_frame, write_frame, ExecRequest, ExecResponse};
pub use tiers::fork::ForkWorker;
#[cfg(unix)]
pub use tiers::pool::{PooledTransport, PooledWorker, WellspringPool};
pub use tiers::probe::probe_modules;
pub use tiers::subinterp::SubInterpWorker;
pub use tiers::subprocess::SubprocessWorker;
pub use transport::{PipeTransport, ReadyInfo, ShimTransport};
pub use watermark::{Watermark, WatermarkId};
pub use watermark_stack::WatermarkStack;
pub use worker::Worker;
pub use worker_caps::WorkerCaps;
