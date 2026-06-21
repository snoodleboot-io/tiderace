//! ADR-E011 ② go/no-go spike — **one embedded CPython, hosted by Rust over FFI**.
//!
//! Rebuts ADR-010 (which rejected *N PEP-684 subinterpreters per process* because single-phase-init
//! C extensions segfault the whole process). The claim here is narrower and different: **one** main
//! interpreter, embedded via PyO3, with the control plane being **FFI calls** (Rust ⇄ Python values)
//! instead of pipe frames. tiderace/engine-core is untouched — this is a disposable crate.
//!
//! **No pytest.** Per ADR-E001 the Rust side *is* the framework; CPython only executes user
//! test/fixture *bodies*. So this spike drives riptide's OWN executor in-process — import the user
//! module, call the bare `test_*` object, catch `AssertionError` — exactly what `engine/py-shim/shim.py`
//! does today (`getattr(module, name)(**args)` + `except AssertionError`), just in-process over FFI
//! instead of over a pipe. pytest is never run.
//!
//! It demonstrates, in a single process, with NO `fork`/`exec`/pipe:
//!   [0] Rust boots one embedded interpreter and reads its identity back.
//!   [A] `import _decimal` — the EXACT stdlib single-phase-init C-ext that core-dumped ADR-010's
//!       subinterpreters — and hammer it. In one interpreter it is a normal cached import: no crash.
//!   [B1] Rust drives stdlib `unittest.TestCase.run()` at method granularity (ADR-E001's unittest path).
//!   [B2] Rust drives riptide's OWN native executor — call bare `test_*` bodies, catch `AssertionError`
//!        — and pulls per-test `(name, outcome, detail)` back as **Rust values**: the
//!        `InProcessTransport::exchange` shape, no pytest.

use std::ffi::CString;

use pyo3::prelude::*;
use pyo3::types::PyModule;

/// The Python-side harness. Pure functions Rust calls over FFI; each returns plain values Rust
/// extracts into native types (no serialization, no bytes).
const HARNESS: &str = r#"
import sys, unittest
from decimal import Decimal          # decimal is backed by the _decimal C extension


def interp_info():
    # One main interpreter — not a subinterpreter (the thing ADR-010 multiplied and crashed).
    return (sys.version.split()[0], sys.executable, sys.implementation.name)


def decimal_torture():
    # ADR-010 repro: a subinterpreter touching _decimal → "mpd_setminalloc ... a second time"
    # → munmap_chunk(): invalid pointer → core dump. Here, in ONE interpreter, it just works.
    import _decimal  # noqa: F401  (re-import: cached, no re-init of the single-phase module)
    a = Decimal("1.1") + Decimal("2.2")
    total = sum((Decimal(i) / Decimal(7) for i in range(5000)), Decimal(0))
    return (str(a), len(str(total)))


def run_unittest():
    class T(unittest.TestCase):
        def test_pass(self):
            self.assertEqual(2 + 2, 4)
        def test_decimal_cext(self):
            self.assertEqual(Decimal("0.1") * 3, Decimal("0.3"))
        def test_intentional_fail(self):
            self.assertEqual(1, 2)          # proves failure capture, not just happy path
    suite = unittest.TestLoader().loadTestsFromTestCase(T)
    res = unittest.TestResult()
    suite.run(res)
    return (res.testsRun, len(res.failures), len(res.errors))


def run_native(path):
    # riptide's OWN executor — NOT pytest. This mirrors engine/py-shim/shim.py exactly: import the
    # user module, call each bare `test_*` object, catch AssertionError. Rust owns collection +
    # fixtures + scopes; CPython only runs the body. Returns per-test (name, outcome, detail).
    import importlib.util
    spec = importlib.util.spec_from_file_location("user_tests", path)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    out = []
    for name in sorted(n for n in dir(mod) if n.startswith("test_")):
        try:
            getattr(mod, name)()                       # call the bare test body (no pytest)
            out.append((name, "passed", ""))
        except AssertionError as exc:
            out.append((name, "failed", str(exc) or "assert"))
        except Exception as exc:                       # noqa: BLE001
            out.append((name, "error", f"{type(exc).__name__}: {exc}"))
    return out
"#;

/// A plain-Python test module riptide's native executor runs by calling the bodies directly.
/// No `import pytest`, no decorators — just `assert`, the semantics riptide reimplements in Rust.
const TEST_PY: &str = r#"
def test_addition():
    assert 1 + 1 == 2

def test_upper():
    assert "ab".upper() == "AB"

def test_intentional_fail():
    assert sum([1, 2, 3]) == 7, "sum is 6, not 7"
"#;

fn main() -> PyResult<()> {
    let test_path = std::env::temp_dir().join("inproc_spike_test.py");
    std::fs::write(&test_path, TEST_PY).expect("write temp pytest module");

    println!("=== ADR-E011 ② spike: one embedded CPython, Rust host, FFI control plane ===\n");

    Python::with_gil(|py| -> PyResult<()> {
        let code = CString::new(HARNESS).expect("harness has no interior NUL");
        let m = PyModule::from_code(py, code.as_c_str(), c"harness.py", c"harness")?;

        // [0] Rust ⇄ one embedded interpreter.
        let (ver, exe, impl_name): (String, String, String) =
            m.getattr("interp_info")?.call0()?.extract()?;
        println!("[0] embedded interpreter: {impl_name} {ver}");
        println!("    sys.executable = {exe}");
        println!("    (single MAIN interpreter — not a subinterpreter)\n");

        // [A] The ADR-010 crasher, in one interpreter: no segfault.
        let (sum_11_22, digits): (String, usize) = m.getattr("decimal_torture")?.call0()?.extract()?;
        println!("[A] _decimal C-ext in-process (ADR-010's segfault module):");
        println!("    Decimal(1.1)+Decimal(2.2) = {sum_11_22}");
        println!("    5000-term Decimal reduction → {digits}-digit result, NO crash ✓\n");

        // [B1] Real unittest in-process; structured result extracted into Rust ints.
        let (ran, failed, errored): (i64, i64, i64) =
            m.getattr("run_unittest")?.call0()?.extract()?;
        println!("[B1] in-process unittest → Rust values: ran={ran} failed={failed} errored={errored}");
        println!("     (the 1 expected failure is faithfully captured across FFI)\n");

        // [B2] riptide's OWN native executor (no pytest): call bare test bodies, catch AssertionError,
        // pull per-test (name, outcome, detail) back as Rust values — the exchange shape.
        let path_str = test_path.to_str().expect("utf8 temp path");
        let native: Vec<(String, String, String)> =
            m.getattr("run_native")?.call1((path_str,))?.extract()?;
        println!("\n[B2] riptide native executor in-process (NO pytest) → Rust values:");
        for (name, outcome, detail) in &native {
            let suffix = if detail.is_empty() {
                String::new()
            } else {
                format!("  ({detail})")
            };
            println!("     {name:<22} {outcome}{suffix}");
        }

        // Verdict: embed ✓, C-ext ✓, unittest path ✓, native executor reports the right per-test verdicts.
        let native_ok = native
            == [
                ("test_addition", "passed", ""),
                ("test_intentional_fail", "failed", "sum is 6, not 7"),
                ("test_upper", "passed", ""),
            ]
            .map(|(n, o, d)| (n.to_string(), o.to_string(), d.to_string()));
        let go = ran == 3 && failed == 1 && errored == 0 && native_ok;
        println!(
            "\n=== VERDICT: {} ===",
            if go {
                "GO — one interpreter + FFI runs riptide's OWN executor (no pytest) AND the C-ext that crashed subinterpreters"
            } else {
                "NO-GO — unexpected result, investigate"
            }
        );
        Ok(())
    })?;

    let _ = std::fs::remove_file(&test_path);
    Ok(())
}
