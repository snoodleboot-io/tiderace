//! `engine-core` — the pure-Rust Python test engine library.
//!
//! The engine owns collection, the domain model, scheduling, execution, caching, and reporting;
//! Python is only an execution substrate (no pytest underneath). See
//! `planning/current/pure-rust-test-engine/design/` for the full design.
//!
//! Phase 2 scope: the domain vocabulary ([`domain`]) and test discovery ([`collection`]).
//! Execution ([`exec`]) is wired in the same phase; later phases add fixtures, cache, scheduler,
//! daemon, and reporters behind the trait seams.

pub mod cache;
pub mod collection;
pub mod coverage;
pub mod domain;
pub mod error;
pub mod exec;
pub mod fixtures;
pub mod hooks;
pub mod impact;
pub mod reporter;
pub mod scheduler;

pub use error::EngineError;
