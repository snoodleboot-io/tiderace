//! Scheduler (Phase 6, design 06, ADR-E010) — duration-aware, scope-locality bin-packing.
//!
//! Sits between cache/impact filtering and execution: decides which worker runs which tests, in which
//! order, reconciling two objectives that pull apart — **makespan** (balance work evenly) and
//! **snapshot reuse** (co-locate a scope's tests so its per-worker snapshot is built once, not rebuilt
//! on every worker the tests scatter to). The [`LocalityScheduler`] packs with both; the
//! [`RoundRobinScheduler`] is the locality-blind baseline it must beat.
//!
//! One type per file (ADR-E005): [`ScheduledTest`], [`WorkerBatch`], [`ScheduleInput`], the
//! [`Scheduler`] trait, [`LocalityScheduler`], [`RoundRobinScheduler`].

mod locality_scheduler;
mod round_robin_scheduler;
mod scheduled_test;
#[allow(clippy::module_inception)]
// file name = snake_case of the `Scheduler` trait (project convention)
mod scheduler;
mod worker_batch;

pub use locality_scheduler::LocalityScheduler;
pub use round_robin_scheduler::RoundRobinScheduler;
pub use scheduled_test::ScheduledTest;
pub use scheduler::{makespan, ScheduleInput, Scheduler};
pub use worker_batch::WorkerBatch;
