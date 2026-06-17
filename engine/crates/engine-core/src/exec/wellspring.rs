use std::io::BufReader;
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::Value;

use crate::error::{EngineError, Result};
use crate::exec::shim_protocol::{read_frame, write_frame, ExecRequest, ExecResponse};

/// A warm Python parent process: imports the project once, then forks a pristine copy-on-write
/// child per test (ADR-E003). Owns the Rust↔shim pipe.
pub struct Wellspring {
    child: Child,
    /// `Option` so [`Drop`] can close stdin (→ shim EOF/exit) *before* reaping, avoiding deadlock.
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
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
        let mut stdout = BufReader::new(stdout);

        let ready: Value = read_frame(&mut stdout)?
            .ok_or_else(|| EngineError::Exec("wellspring sent no ready frame".into()))?;
        if ready.get("ready").and_then(Value::as_bool) != Some(true) {
            return Err(EngineError::Exec(format!(
                "wellspring failed to warm: {ready}"
            )));
        }
        let pid = ready.get("pid").and_then(Value::as_i64).unwrap_or(-1);

        Ok(Self {
            child,
            stdin: Some(stdin),
            stdout,
            pid,
        })
    }

    /// The Wellspring process id (parent of all per-test forks).
    pub fn pid(&self) -> i64 {
        self.pid
    }

    /// Run one test; the shim forks a pristine child to execute it.
    pub fn run_one(&mut self, req: &ExecRequest) -> Result<ExecResponse> {
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| EngineError::Exec("wellspring already shut down".into()))?;
        write_frame(stdin, req)?;
        read_frame(&mut self.stdout)?
            .ok_or_else(|| EngineError::Exec("wellspring closed mid-run".into()))
    }
}

impl Drop for Wellspring {
    fn drop(&mut self) {
        // Close stdin first (EOF → shim exits cleanly), THEN reap — order matters to avoid a hang.
        drop(self.stdin.take());
        let _ = self.child.wait();
    }
}
