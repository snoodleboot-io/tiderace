//! Sub-interpreter safety detection (ADR-E015, TID-9). Drives the shim's `--probe` mode to classify
//! each test **module** as safe/unsafe to run on the sub-interpreter execution tier — the foundation the
//! `SubInterpWorker` (Phase 2) and Windows routing (Phase 3) build on. No tests are executed here; this
//! only imports each module in an isolated sub-interpreter and reports whether it loads.

use std::collections::BTreeMap;
use std::io::BufReader;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::exec::{read_frame, write_frame};
use serde_json::{json, Value};

/// Classify each module (rel path, e.g. `pkg/test_x.py`) by driving `python <shim> <root> --probe`.
/// `Some(true)` = safe (loads in an isolated sub-interpreter), `Some(false)` = unsafe (a single-phase
/// C-extension like numpy), `None` = undeterminable (probe API unavailable on CPython < 3.14 → the
/// caller falls back to fork/subprocess, which is always sound).
pub fn probe_modules(
    python: &str,
    shim: &Path,
    root: &Path,
    modules: &[String],
) -> Result<BTreeMap<String, Option<bool>>, String> {
    let mut child = Command::new(python)
        .arg(shim)
        .arg(root)
        .arg("--probe")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to launch probe: {e}"))?;
    let mut stdin = child.stdin.take().ok_or("probe stdin unavailable")?;
    let mut stdout = BufReader::new(child.stdout.take().ok_or("probe stdout unavailable")?);

    // Readiness handshake (mirrors the serve/wellspring protocol).
    let _ready: Option<Value> = read_frame(&mut stdout).map_err(|e| format!("probe ready: {e}"))?;

    let mut out = BTreeMap::new();
    for m in modules {
        write_frame(&mut stdin, &json!({ "module": m }))
            .map_err(|e| format!("probe write: {e}"))?;
        let resp: Value = read_frame(&mut stdout)
            .map_err(|e| format!("probe read: {e}"))?
            .ok_or("probe closed mid-run")?;
        // `safe` is true / false / null (undeterminable).
        let safe = resp
            .get("safe")
            .and_then(|v| if v.is_null() { None } else { v.as_bool() });
        out.insert(m.clone(), safe);
    }
    drop(stdin); // EOF → the probe process exits
    let _ = child.wait();
    Ok(out)
}
