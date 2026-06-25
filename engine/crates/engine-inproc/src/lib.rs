//! ② in-process / FFI execution backend (ADR-E011 ②, ADR-E013).
//!
//! [`InProcessTransport`] is the third implementation of `engine_core::exec::ShimTransport` (beside the
//! production `PipeTransport` and the test double). Instead of spawning `python shim.py` as a
//! **subprocess** and exchanging length-prefixed JSON over **pipes**, it embeds **one** CPython
//! interpreter via PyO3, imports the project **once** (the in-process wellspring), and drives the
//! shim's own `Engine` by **FFI call** — `engine.run(node_id, …)` returns a Python dict that becomes an
//! `ExecResponse` Rust value directly. No subprocess, no pipe/JSON control plane.
//!
//! **Isolation is unchanged (ADR-E013, fork-from-embedded):** the shim's `Engine.run` still `os.fork()`s
//! a pristine COW child per test internally — that fork now originates from the embedded interpreter.
//! The Rust parent must stay single-threaded at the fork point (drive this transport from one thread).
//!
//! This crate is **excluded from the default workspace build** (links libpython); build it explicitly
//! with `PYO3_PYTHON=<python-with-libpython> cargo build --manifest-path crates/engine-inproc/Cargo.toml`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use engine_core::error::{EngineError, Result};
use engine_core::exec::{ExecRequest, ExecResponse, ReadyInfo, ShimTransport};
use pyo3::prelude::*;
use pyo3::types::PyDict;

/// An embedded-interpreter [`ShimTransport`]. Holds the warm shim `Engine` as a Python object; each
/// [`exchange`](ShimTransport::exchange) calls `engine.run(...)` by FFI.
pub struct InProcessTransport {
    root: PathBuf,
    /// Extra `sys.path` entries (the `py-shim` dir, and `py-riptide` so `import riptide.*` resolves).
    py_paths: Vec<PathBuf>,
    coverage: bool,
    /// The warm `shim.Engine` instance, or `None` until [`ready`](ShimTransport::ready).
    engine: Option<Py<PyAny>>,
    pid: i64,
}

impl InProcessTransport {
    /// `root` is the corpus dir (placed on `sys.path`); `py_paths` are the shim + riptide package dirs.
    pub fn new(root: impl Into<PathBuf>, py_paths: Vec<PathBuf>, coverage: bool) -> Self {
        Self {
            root: root.into(),
            py_paths,
            coverage,
            engine: None,
            pid: -1,
        }
    }

    fn boot(&mut self) -> Result<()> {
        Python::attach(|py| -> PyResult<()> {
            let sys = py.import("sys")?;
            let path = sys.getattr("path")?;
            // Corpus root first, then the engine's python dirs, so `import shim` / `riptide.*` resolve.
            for p in std::iter::once(&self.root).chain(self.py_paths.iter()) {
                path.call_method1("insert", (0, p.to_string_lossy().as_ref()))?;
            }
            let shim = py.import("shim")?;
            let root = self.root.to_string_lossy();
            shim.call_method1("_preimport", (root.as_ref(),))?;
            let reg = shim.call_method1("_discover", (root.as_ref(),))?;

            let kwargs = PyDict::new(py);
            kwargs.set_item("no_fork", false)?; // fork-from-embedded isolation (ADR-E013)
            kwargs.set_item("root", root.as_ref())?;
            kwargs.set_item("coverage", self.coverage)?;
            let engine = shim.getattr("Engine")?.call((reg,), Some(&kwargs))?;

            self.pid = py.import("os")?.call_method0("getpid")?.extract()?;
            self.engine = Some(engine.unbind());
            Ok(())
        })
        .map_err(|e| EngineError::Exec(format!("in-process boot failed: {e}")))
    }
}

impl ShimTransport for InProcessTransport {
    fn ready(&mut self) -> Result<ReadyInfo> {
        if self.engine.is_none() {
            self.boot()?;
        }
        Ok(ReadyInfo { pid: self.pid })
    }

    fn exchange(&mut self, req: &ExecRequest<'_>) -> Result<ExecResponse> {
        let engine = self
            .engine
            .as_ref()
            .ok_or_else(|| EngineError::Exec("in-process transport not ready".into()))?;
        Python::attach(|py| -> PyResult<ExecResponse> {
            let res = engine
                .bind(py)
                .call_method1("run", (req.node_id, req.style, req.deadline_ms))?;
            let node_id: String = res.get_item("node_id")?.extract()?;
            let outcome: String = res.get_item("outcome")?.extract()?;
            let detail: String = res
                .get_item("detail")
                .ok()
                .and_then(|d| d.extract().ok())
                .unwrap_or_default();
            let coverage = extract_coverage(&res)?;
            Ok(ExecResponse {
                node_id,
                outcome,
                detail,
                coverage,
            })
        })
        .map_err(|e| EngineError::Exec(format!("in-process exchange failed: {e}")))
    }
}

/// Pull the optional `coverage` dict (`{rel_path: [lines]}`) off the run result, or empty if absent.
fn extract_coverage(res: &Bound<'_, PyAny>) -> PyResult<BTreeMap<String, Vec<u32>>> {
    match res.call_method1("get", ("coverage",)) {
        Ok(v) if !v.is_none() => v.extract(),
        _ => Ok(BTreeMap::new()),
    }
}

/// Convenience: the engine's `py-shim` and `py-riptide` dirs given the repo's `engine/` directory.
pub fn engine_py_paths(engine_dir: &Path) -> Vec<PathBuf> {
    vec![engine_dir.join("py-shim"), engine_dir.join("py-riptide")]
}
