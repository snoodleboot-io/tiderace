//! The execution tiers: each one a way of running a batch against a shim process, behind the
//! same [`Worker`](crate::exec::Worker) seam.
//!
//! * [`fork`] — one warm wellspring, a forked child per test (Unix).
//! * [`pool`] — one imported image, N forked workers connected over sockets (Unix).
//! * [`subprocess`] — the no-fork tier: one shim process, tests run in it under snapshot/restore.
//! * [`subinterp`] — a pool of sub-interpreters in one process (CPython 3.14+).
//! * [`probe`] — no tests: classifies modules for the sub-interpreter tier.

pub mod fork;
#[cfg(unix)]
pub mod fork_tier;
#[cfg(unix)]
pub mod pool;
pub mod probe;
pub mod subinterp;
pub mod subprocess;
