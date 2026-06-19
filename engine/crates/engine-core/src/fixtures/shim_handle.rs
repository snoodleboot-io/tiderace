//! `ShimHandle` — an opaque, serializable reference to a registered Python continuation in the shim.
//!
//! The teardown half of a yield-style fixture lives as a Python generator suspended at its `yield`.
//! Rust never holds the Python object; it holds this **handle** (an integer token the shim assigns)
//! and owns only the *ordering* of teardown. The shim owns *invoking* the continuation (design 04
//! §1.1). Pure data — fully defined.

use serde::{Deserialize, Serialize};

/// An opaque token identifying a teardown continuation registered in the shim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ShimHandle(u64);

impl ShimHandle {
    /// Wrap a raw token the shim assigned to a registered continuation.
    pub fn new(raw: u64) -> Self {
        Self(raw)
    }

    /// The raw token (sent back over the wire to invoke the continuation).
    pub fn get(self) -> u64 {
        self.0
    }
}
