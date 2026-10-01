//! The daemon's one error type (TID-119). Every handler function used to return
//! `Result<_, String>` through `map_err(|e| format!(…))`, and the RPC dispatcher repeated the
//! `Ok → response, Err → Error { message }` mapping per arm.

use std::fmt;

use engine_core::EngineError;

#[derive(Debug)]
pub enum DaemonError {
    /// The engine refused or failed: launch, handshake, a lost worker, collection.
    Engine(EngineError),
    /// The daemon's own I/O: the state file, the socket, the log.
    Io(std::io::Error),
    /// A daemon-level condition with no type of its own.
    Message(String),
}

impl fmt::Display for DaemonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DaemonError::Engine(e) => write!(f, "{e}"),
            DaemonError::Io(e) => write!(f, "{e}"),
            DaemonError::Message(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for DaemonError {}

impl From<EngineError> for DaemonError {
    fn from(e: EngineError) -> Self {
        DaemonError::Engine(e)
    }
}

impl From<std::io::Error> for DaemonError {
    fn from(e: std::io::Error) -> Self {
        DaemonError::Io(e)
    }
}

impl From<String> for DaemonError {
    fn from(m: String) -> Self {
        DaemonError::Message(m)
    }
}

/// Convenience alias for the daemon's fallible operations.
pub type Result<T> = std::result::Result<T, DaemonError>;
