//! Sub-interpreter safety detection (ADR-E015, TID-9). Drives the shim's `--probe` mode to classify
//! each test **module** as safe/unsafe to run on the sub-interpreter execution tier — the foundation the
//! `SubInterpWorker` (Phase 2) and Windows routing (Phase 3) build on. No tests are executed here; this
//! only imports each module in an isolated sub-interpreter and reports whether it loads.

use std::collections::BTreeMap;
use std::path::Path;

use crate::exec::process::{ShimLaunch, ShimMode, ShimTarget};
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
    let target = ShimTarget::new(python, shim, root);
    let mut probe = ShimLaunch::new(&target, ShimMode::Probe)
        .spawn()
        .map_err(|e| e.to_string())?;
    probe.ready().map_err(|e| format!("probe ready: {e}"))?;

    let mut out = BTreeMap::new();
    for m in modules {
        write_frame(
            probe.stdin().map_err(|e| e.to_string())?,
            &json!({ "module": m }),
        )
        .map_err(|e| format!("probe write: {e}"))?;
        let resp: Value = read_frame(probe.stdout().map_err(|e| e.to_string())?)
            .map_err(|e| format!("probe read: {e}"))?
            .ok_or("probe closed mid-run")?;
        // `safe` is true / false / null (undeterminable).
        let safe = resp
            .get("safe")
            .and_then(|v| if v.is_null() { None } else { v.as_bool() });
        out.insert(m.clone(), safe);
    }
    // Dropped: stdin closed (EOF → the probe process exits), then reaped.
    Ok(out)
}
