//! Sub-interpreter safety detection (ADR-E015, TID-9). Drives the shim's `--probe` mode to classify
//! each test **module** as safe/unsafe to run on the sub-interpreter execution tier — the foundation the
//! `SubInterpWorker` (Phase 2) and Windows routing (Phase 3) build on. No tests are executed here; this
//! only imports each module in an isolated sub-interpreter and reports whether it loads.

use std::collections::BTreeMap;
use std::io::BufReader;
use std::path::Path;
use std::process::{Command, Stdio};

use engine_core::exec::{read_frame, write_frame};
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

#[cfg(test)]
mod tests {
    use super::*;
    use engine_core::testing::skip_live;
    use std::path::PathBuf;

    fn repo_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../..")
            .canonicalize()
            .expect("repo root")
    }
    // Needs the fx venv (CPython 3.14 + numpy) for the unsafe case; self-skips otherwise.
    fn venv_python() -> Option<String> {
        let p = repo_root().join(".tiderace-fx-venv/bin/python");
        p.exists().then(|| p.to_string_lossy().into_owned())
    }
    fn shim() -> PathBuf {
        repo_root().join("engine/py-shim/shim.py")
    }

    #[test]
    fn classifies_pure_safe_and_numpy_unsafe() {
        let Some(python) = venv_python() else {
            skip_live("`.tiderace-fx-venv` not present");
            return;
        };
        let dir = std::env::temp_dir().join(format!("tiderace_probe_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("test_pure.py"),
            "def test_a():\n    assert 1 == 1\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("test_np.py"),
            "import numpy\ndef test_n():\n    assert int(numpy.array([1]).sum()) == 1\n",
        )
        .unwrap();

        let modules = vec!["test_pure.py".to_string(), "test_np.py".to_string()];
        let v = probe_modules(&python, &shim(), &dir, &modules).expect("probe runs");

        // On CPython 3.14 (the fx venv) verdicts are determinate; on < 3.14 the probe reports None and
        // the caller falls back to fork — assert the determinate results only when we got them.
        match v.get("test_pure.py") {
            Some(Some(true)) => {
                assert_eq!(
                    v.get("test_np.py"),
                    Some(&Some(false)),
                    "numpy module is unsafe for the sub-interpreter tier"
                );
            }
            Some(None) => {
                eprintln!("skipping assertions: concurrent.interpreters unavailable (<3.14)")
            }
            other => panic!("unexpected verdict for the pure module: {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
