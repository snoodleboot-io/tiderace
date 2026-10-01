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

mod hashing;
mod memory;
mod parallel_runner;
mod phase_timer;
mod run_notes;
mod run_plan;
mod scheduler_kind;
mod verdicts;
mod worker_strategy;

pub use hashing::{digest, hash_bytes, hash_file, hash_file_or_missing, MISSING};
pub use memory::{
    available_memory_bytes, memory_limit_mb_from_env, process_rss_bytes, workers_by_memory,
    MemorySizing,
};
pub use parallel_runner::{run_parallel, run_parallel_with_notes};
#[cfg(unix)]
pub use parallel_runner::{run_parallel_with_pool, run_parallel_with_pool_notes};
pub use phase_timer::PhaseTimer;
pub use run_notes::{RunNotes, RunOutcome};
pub use run_plan::{
    default_workers, ForkOptions, Learned, RunPlan, Sharding, WorkerCount, DEFAULT_DEADLINE_MS,
};
pub use scheduler_kind::SchedulerKind;
pub use verdicts::{
    changed_files, record_durations, PersistedState, RecordedOutcome, TestRecord, VerdictStore,
    STATE_FILE,
};
pub use worker_strategy::WorkerStrategy;
