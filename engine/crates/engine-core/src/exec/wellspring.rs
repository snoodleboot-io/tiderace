use std::io::BufReader;
use std::path::Path;
use std::process::{Child, Command, Stdio};

use crate::error::{EngineError, Result};
use crate::exec::shim_protocol::{ExecRequest, ExecResponse};
use crate::exec::transport::{Live, PipeTransport, ShimTransport};

/// A warm Python parent process: imports the project once, then forks a pristine copy-on-write
/// child per test (ADR-E003). Owns the Rust↔shim [`PipeTransport`].
pub struct Wellspring {
    child: Child,
    /// The framed pipe to the shim. Its write half is closed on [`Drop`] (→ shim EOF/exit) *before*
    /// the child is reaped, avoiding a shutdown deadlock.
    transport: Live,
    pid: i64,
}

impl Wellspring {
    /// Launch `python <shim> <root>` and complete the readiness handshake. `root` is placed on the
    /// shim's `sys.path`, so collected node ids resolve as module paths relative to it.
    pub fn launch(python: &str, shim: &Path, root: &Path) -> Result<Self> {
        Self::launch_with(python, shim, root, false)
    }

    /// As [`launch`](Wellspring::launch), but asks the shim for snapshot/restore.
    ///
    /// This is the precondition for the optimistic in-process ladder: without it a test taking the
    /// no-fork path gets no COW copy AND no snapshot, so its mutations to module globals persist
    /// into every later test on that module. The shim reads `--restore` / `TIDERACE_RESTORE`, and
    /// passing the flag explicitly is what keeps correctness off the caller's environment — the same
    /// reasoning `SubprocessWorker` already applies to its own launch.
    pub fn launch_with(python: &str, shim: &Path, root: &Path, restore: bool) -> Result<Self> {
        Self::launch_selected(python, shim, root, restore, None)
    }

    /// As [`launch_with`](Wellspring::launch_with), importing only the modules named in `modules`
    /// — a file of suite-relative paths, one per line — and the conftests above them (TID-75).
    ///
    /// The shim's start-up imports every test module before a worker exists: 4s on pirn-agents,
    /// which was the whole of a one-test run after an edit. `None` keeps the full import, which is
    /// what a full run needs and what every caller that has no selection gets.
    pub fn launch_selected(
        python: &str,
        shim: &Path,
        root: &Path,
        restore: bool,
        modules: Option<&Path>,
    ) -> Result<Self> {
        let mut cmd = Command::new(python);
        cmd.arg(shim).arg(root);
        if restore {
            cmd.arg("--restore");
        }
        if let Some(file) = modules {
            cmd.arg("--modules").arg(file);
        }
        let mut child = cmd
            // Pin native thread pools — threaded BLAS/OMP + fork() is a known hazard (Phase-1
            // learning; generalized as a thread/reinit policy in Phase 3).
            .env("OPENBLAS_NUM_THREADS", "1")
            .env("OMP_NUM_THREADS", "1")
            .env("MKL_NUM_THREADS", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|e| EngineError::Exec(format!("failed to launch wellspring: {e}")))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| EngineError::Exec("wellspring stdin unavailable".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| EngineError::Exec("wellspring stdout unavailable".into()))?;

        let mut transport = PipeTransport::new(stdin, BufReader::new(stdout));
        let ready = transport.ready()?;

        Ok(Self {
            child,
            transport,
            pid: ready.pid,
        })
    }

    /// The Wellspring process id (parent of all per-test forks).
    pub fn pid(&self) -> i64 {
        self.pid
    }

    /// Run one test; the shim forks a pristine child to execute it.
    pub fn run_one(&mut self, req: &ExecRequest) -> Result<ExecResponse> {
        self.transport.exchange(req)
    }

    /// The shim transport, for the batch run loop ([`crate::exec::transport::run_batch`]).
    pub(crate) fn transport_mut(&mut self) -> &mut Live {
        &mut self.transport
    }
}

impl Drop for Wellspring {
    fn drop(&mut self) {
        // Close stdin first (EOF → shim exits cleanly), THEN reap — order matters to avoid a hang.
        self.transport.close_input();
        let _ = self.child.wait();
    }
}
