use std::io::{BufReader, Read};
use std::process::{Child, ChildStdin, ChildStdout};

use serde_json::Value;

use crate::error::{EngineError, Result};
use crate::exec::process::ShimMode;
use crate::exec::shim_protocol::{read_frame, ready_info};
use crate::exec::transport::ReadyInfo;

/// A running shim and whatever of its pipes the engine still holds.
///
/// Dropping it ends the process the way every tier did by hand: close its stdin first (EOF is
/// how the shim learns to run its wider-scope finalizers and exit), kill it if it was marked
/// lost — it is inside a test that will not end — and reap it. The write half is closed
/// *before* the reap, or a shim blocked in `read` and an engine blocked in `wait` would wait
/// for each other.
pub struct ShimProcess {
    child: Child,
    mode: ShimMode,
    stdin: Option<ChildStdin>,
    stdout: Option<BufReader<ChildStdout>>,
    lost: bool,
}

impl ShimProcess {
    pub(crate) fn new(mut child: Child, mode: ShimMode) -> Self {
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().map(BufReader::new);
        Self {
            child,
            mode,
            stdin,
            stdout,
            lost: false,
        }
    }

    /// The process id.
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// What this process was launched as.
    pub fn mode(&self) -> &ShimMode {
        &self.mode
    }

    /// Take both pipes, for a transport to own. Available once: the second call is an error,
    /// as is a call on a process launched without pipes.
    pub fn take_pipes(&mut self) -> Result<(ChildStdin, BufReader<ChildStdout>)> {
        let stdin = self.stdin.take().ok_or(EngineError::AlreadyShutDown {
            what: self.mode.name(),
        })?;
        let stdout = self.stdout.take().ok_or(EngineError::AlreadyShutDown {
            what: self.mode.name(),
        })?;
        Ok((stdin, stdout))
    }

    /// The write half, while this process still holds it.
    pub fn stdin(&mut self) -> Result<&mut ChildStdin> {
        self.stdin.as_mut().ok_or(EngineError::AlreadyShutDown {
            what: self.mode.name(),
        })
    }

    /// The read half, while this process still holds it.
    pub fn stdout(&mut self) -> Result<&mut BufReader<ChildStdout>> {
        self.stdout.as_mut().ok_or(EngineError::AlreadyShutDown {
            what: self.mode.name(),
        })
    }

    /// Read the readiness frame — the first frame the shim sends, after it has imported the
    /// suite — on a process whose pipes this still holds. For a process whose pipes a
    /// transport took, the transport's `ready()` does the same.
    pub fn ready(&mut self) -> Result<ReadyInfo> {
        let what = self.mode.name();
        let frame: Option<Value> = read_frame(self.stdout()?)?;
        let Some(frame) = frame else {
            // Exited before it was ready: the Python traceback on stderr says why.
            let _ = self.child.wait();
            return Err(EngineError::ExitedBeforeReady { what });
        };
        ready_info(frame)
    }

    /// The exit status, if the process has already exited; `None` while it runs.
    pub fn exited(&mut self) -> Option<std::process::ExitStatus> {
        self.child.try_wait().ok().flatten()
    }

    /// Close the write half: EOF, which is how the shim is told the run is over.
    pub fn close_input(&mut self) {
        self.stdin.take();
    }

    /// Mark the process lost — it stopped answering inside a test — so the drop kills it
    /// rather than waiting for an exit that will not come.
    pub fn mark_lost(&mut self) {
        self.lost = true;
    }

    /// Kill the process now; the drop still reaps it.
    pub fn kill(&mut self) {
        let _ = self.child.kill();
    }
}

impl std::fmt::Debug for ShimProcess {
    /// Hand-written: the pipe handles are not `Debug`; what identifies the process is its mode,
    /// its pid and whether it was lost.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShimProcess")
            .field("mode", &self.mode)
            .field("pid", &self.child.id())
            .field("lost", &self.lost)
            .field(
                "holds_pipes",
                &(self.stdin.is_some() || self.stdout.is_some()),
            )
            .finish()
    }
}

impl Drop for ShimProcess {
    fn drop(&mut self) {
        if self.lost {
            let _ = self.child.kill();
        }
        self.stdin.take();
        if let Some(stdout) = self.stdout.as_mut() {
            // Drain what the shim still writes on its way out, so it is never blocked on a
            // full pipe between EOF and exit.
            let mut sink = Vec::new();
            let _ = stdout.get_mut().read_to_end(&mut sink);
        }
        let _ = self.child.wait();
    }
}
