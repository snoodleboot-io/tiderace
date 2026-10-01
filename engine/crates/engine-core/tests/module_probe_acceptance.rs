//! The sub-interpreter safety probe against a real interpreter: a pure module is safe, a numpy
//! module is not. Needs the fx venv (CPython 3.14 + numpy); self-skips otherwise.

use engine_core::exec::probe_modules;
use engine_core::testing::{require_python, scratch, shim, PythonNeeds};

#[test]
fn classifies_pure_safe_and_numpy_unsafe() {
    let Some(python) = require_python(PythonNeeds::FxVenv) else {
        return;
    };
    let dir = scratch("probe");
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
