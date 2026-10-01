//! Typed engine errors. No panics in library code (per Rust conventions).

use std::time::Duration;

use thiserror::Error;

/// The top-level engine error. Variants map to the subsystem that produced them.
///
/// Note the distinction the whole engine relies on: an `EngineError` is an *engine/infrastructure*
/// failure (could not collect, could not talk to the substrate). A *test* that errors is **not** an
/// `EngineError` — it is a [`crate::domain::Outcome::Error`] carried on a `TestResult`.
///
/// The `Display` text of each variant is what the CLI prints and what the acceptance tests
/// match on; it is kept stable as the variants grow (TID-115).
#[derive(Debug, Error)]
pub enum EngineError {
    /// Test discovery failed (unreadable root, bad pattern, …).
    #[error("collection failed: {0}")]
    Collection(String),

    /// The shim process could not be started: `what` names its mode.
    #[error("failed to launch the {what}: {source}")]
    Launch {
        what: &'static str,
        #[source]
        source: std::io::Error,
    },

    /// The shim's first frame was not a readiness frame.
    #[error("shim failed to warm: {frame}")]
    Handshake { frame: String },

    /// The shim sent nothing where its readiness frame was expected.
    #[error("{what} sent no ready frame")]
    NoReadyFrame { what: &'static str },

    /// The shim exited before it was ready: the Python traceback on stderr says why.
    #[error("the {what} exited before it was ready — the Python traceback above says why")]
    ExitedBeforeReady { what: &'static str },

    /// The peer closed the connection with a request outstanding.
    #[error("{what} closed mid-run")]
    PeerClosed { what: &'static str },

    /// A request after the transport's write half was closed.
    #[error("{what} already shut down")]
    AlreadyShutDown { what: &'static str },

    /// A request to a worker already given up on.
    #[error("the worker was lost")]
    WorkerGone,

    /// No reply within the budget: the worker is killed and reported lost (TID-98).
    #[error(
        "no answer from the worker within {:.1}s — its test overran the deadline and the \
         in-process timeout could not interrupt it; the worker is killed and reported lost (TID-98)",
        .budget.as_secs_f64()
    )]
    WorkerLost { budget: Duration },

    /// A frame the engine could not encode or read: too large, not JSON, not the shape expected.
    #[error("{0}")]
    Protocol(String),

    /// A worker driven before it was pointed at an interpreter.
    #[error("{worker} has no target; call with_target")]
    NoTarget { worker: &'static str },

    /// A tier this platform does not have.
    #[error("{0}")]
    Unavailable(String),

    /// Talking to the Python substrate failed in a way none of the above names.
    #[error("execution substrate failed: {0}")]
    Exec(String),

    /// Underlying I/O error.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl From<serde_json::Error> for EngineError {
    fn from(e: serde_json::Error) -> Self {
        EngineError::Protocol(e.to_string())
    }
}

/// Convenience alias for fallible engine operations.
pub type Result<T> = std::result::Result<T, EngineError>;
