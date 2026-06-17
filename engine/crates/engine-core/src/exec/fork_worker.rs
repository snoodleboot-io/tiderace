use std::path::Path;
use std::time::Instant;

use crate::domain::{Outcome, TestItem, TestResult};
use crate::error::Result;
use crate::exec::shim_protocol::ExecRequest;
use crate::exec::wellspring::Wellspring;
use crate::exec::worker::Worker;

/// Default executor (Linux/macOS): one warm [`Wellspring`], fork-per-test (ADR-E003).
pub struct ForkWorker {
    wellspring: Wellspring,
    deadline_ms: u64,
}

impl ForkWorker {
    /// Launch the worker against `root` (the directory placed on the shim's `sys.path`).
    pub fn launch(python: &str, shim: &Path, root: &Path) -> Result<Self> {
        Ok(Self {
            wellspring: Wellspring::launch(python, shim, root)?,
            deadline_ms: 5_000,
        })
    }

    /// Per-test deadline (ms) after which the forked child is killed and reported as `Error`.
    pub fn with_deadline_ms(mut self, ms: u64) -> Self {
        self.deadline_ms = ms;
        self
    }

    /// The underlying Wellspring pid (for diagnostics/tests).
    pub fn wellspring_pid(&self) -> i64 {
        self.wellspring.pid()
    }
}

impl Worker for ForkWorker {
    fn run(&mut self, items: &[TestItem]) -> Result<Vec<TestResult>> {
        let mut results = Vec::with_capacity(items.len());
        for item in items {
            let req = ExecRequest {
                node_id: item.node_id.as_str(),
                style: item.style.wire(),
                deadline_ms: self.deadline_ms,
            };
            let start = Instant::now();
            let resp = self.wellspring.run_one(&req)?;
            let duration_ms = start.elapsed().as_millis() as u64;
            results.push(TestResult::new(
                item.node_id.clone(),
                Outcome::from_wire(&resp.outcome),
                duration_ms,
                resp.detail,
            ));
        }
        Ok(results)
    }
}
