//! Execution substrate — a warm Wellspring forks a pristine child per test over a length-prefixed
//! binary IPC shim (ADR-E002/E003). No pytest underneath.
//!
//! Phase 2 ships the default [`ForkWorker`]; [`Worker`] is the seam behind which the no-fork
//! [`SubprocessWorker`] (ADR-E008), free-threaded, and remote variants land.
//!
//! Fixture scopes live in the shim: the warm image keeps wider-scope fixtures live and tears them
//! down as a worker moves between modules (`tiderace_shim/engine.py`); what the engine sizes is the
//! pool (`runner::memory::workers_by_memory`, TID-106) and the deadlines ([`limits`]). The
//! Phase-3 Rust snapshot-layer and memory-governor machinery that once planned forks from a Rust
//! fixture graph had no caller and was retired (TID-110).

mod knobs;
mod limits;
mod process;
mod results;
mod safe_set_cache;
mod selection;
mod shim_protocol;
mod tier;
pub(crate) mod tiers;
mod transport;
mod worker;

pub use knobs::RunKnobs;
pub use limits::{
    default_parallelism, DEFAULT_DEADLINE_MS, IMPORT_DEADLINE, LOST_WORKER_MARGIN,
    LOST_WORKER_MARGIN_MS, POOL_POLL, WORKER_DEADLINE,
};
pub use process::{reap_lost, BudgetedReader, ShimLaunch, ShimMode, ShimProcess, ShimTarget};
pub use results::NotRun;
pub use safe_set_cache::{SafeModule, SafeSetCache};
pub use selection::{KeywordExpr, Selection, SelectionEnvGuard};
pub use shim_protocol::{read_frame, write_frame, ExecRequest, ExecResponse};
pub use tier::{LaneSeed, TierFactory, WarmImage, WorkerStrategy};
pub use tiers::fork::ForkWorker;
#[cfg(unix)]
pub use tiers::pool::{PooledTransport, PooledWorker, WellspringPool};
pub use tiers::probe::probe_modules;
pub use tiers::subinterp::SubInterpWorker;
pub use tiers::subprocess::SubprocessWorker;
pub use transport::{PipeTransport, ReadyInfo, ShimTransport, WriteHalf};
pub use worker::Worker;
