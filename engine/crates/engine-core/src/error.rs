//! Typed engine errors. No panics in library code (per Rust conventions).

use thiserror::Error;

/// The top-level engine error. Variants map to the subsystem that produced them.
///
/// Note the distinction the whole engine relies on: an `EngineError` is an *engine/infrastructure*
/// failure (could not collect, could not talk to the substrate). A *test* that errors is **not** an
/// `EngineError` — it is a [`crate::domain::Outcome::Error`] carried on a `TestResult`.
#[derive(Debug, Error)]
pub enum EngineError {
    /// Test discovery failed (unreadable root, bad pattern, …).
    #[error("collection failed: {0}")]
    Collection(String),

    /// Talking to / launching the Python substrate failed.
    #[error("execution substrate failed: {0}")]
    Exec(String),

    /// Underlying I/O error.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Convenience alias for fallible engine operations.
pub type Result<T> = std::result::Result<T, EngineError>;
