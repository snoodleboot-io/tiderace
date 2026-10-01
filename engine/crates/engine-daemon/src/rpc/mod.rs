//! The daemon's wire: the request and response shapes, the framed connection loop, and the
//! per-project Unix socket it listens on (design 08, ADR-E007).

pub mod method;
pub mod server;
pub mod socket;
