//! Process-group helpers so a killed test takes its whole subtree with it.
//!
//! A test may spawn its own child processes. Killing only the worker/test PID
//! leaves those grandchildren orphaned. We put each spawned process in its own
//! process group and, on timeout/crash, signal the entire group.

use std::process::{Child, Command};

/// Make the child the leader of a new process group (so the group id == its pid).
pub fn set_process_group(cmd: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    #[cfg(not(unix))]
    {
        let _ = cmd;
    }
}

/// Kill the child **and its process group** (reaping grandchildren), then reap it.
pub fn kill_tree(child: &mut Child) {
    #[cfg(unix)]
    {
        // Negative pid targets the whole process group led by `pid`.
        let pid = child.id() as i32;
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}
