//! Run orchestration (TID-17) — the seam that turns "what to execute" into "how it was executed".
//!
//! The engine has shipped three isolation tiers and two schedulers for some time, but the only
//! consumer that could select among them was the daemon, and it hardwired one of each. The CLI
//! always launched a [`ForkWorker`](crate::exec::ForkWorker) with locality packing, so every
//! measurement anyone took through it described a single configuration while being reported as
//! "tiderace's performance".
//!
//! This module names the choices ([`WorkerStrategy`], [`SchedulerKind`]), bundles them with the
//! rest of a run's configuration ([`RunPlan`], which can [`header`](RunPlan::header) itself so a
//! result states what produced it), and executes them ([`run_parallel`]). Living in `engine-core`
//! rather than the daemon is what lets both front ends share one implementation.
//!
//! One type per file (ADR-E005).

mod parallel_runner;
mod run_plan;
mod scheduler_kind;
mod worker_strategy;

pub use parallel_runner::{locality_key, run_parallel};
pub use run_plan::{default_workers, RunPlan, DEFAULT_DEADLINE_MS};
pub use scheduler_kind::SchedulerKind;
pub use worker_strategy::WorkerStrategy;
