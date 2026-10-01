//! The shim as a process: one way to launch it in any of its modes, one handshake, one shutdown
//! (TID-113). Every execution tier used to build its own `Command`, pin the same thread-count
//! env vars, take the pipes, parse the ready frame and close-then-reap on drop — five copies,
//! four of them slightly different.

mod launch;
mod shim_process;

pub use launch::{ShimLaunch, ShimMode, ShimTarget};
pub use shim_process::ShimProcess;
