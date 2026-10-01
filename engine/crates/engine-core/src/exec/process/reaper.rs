//! Ending a worker the engine has no `Child` handle for — a fork of the pool's imported image,
//! known only by the pid its ready frame reported (TID-93). The one `cfg(unix)` in the process
//! layer: elsewhere there is no such worker.

/// Kill `pid` outright. It is inside a test that will not end, so nothing gentler applies, and
/// its parent — the pool — reaps it.
#[cfg(unix)]
pub fn reap_lost(pid: u32) {
    unsafe extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    // SAFETY: a plain kill(2) on a pid this engine's own pool forked for this run.
    unsafe { kill(pid as i32, 9) };
}

/// No pooled workers off Unix: nothing to kill.
#[cfg(not(unix))]
pub fn reap_lost(_pid: u32) {}
