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
        let mut child = Command::new(python)
            .arg(shim)
            .arg(root)
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
