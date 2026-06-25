//! ② feasibility probe (ADR-E013 step 0): confirm PyO3 links libpython in the workspace and that one
//! embedded interpreter can (A) run a C-extension that crashed subinterpreters and (B) drive riptide's
//! own executor — import a module, call bare `test_*`, catch `AssertionError` — returning native values
//! (the `InProcessTransport::exchange` shape). **No pytest.**

use pyo3::exceptions::PyAssertionError;
use pyo3::prelude::*;
use pyo3::types::PyModule;

fn main() -> PyResult<()> {
    Python::attach(|py| {
        let v = py.version_info();
        println!("[0] embedded interpreter: cpython {}.{}.{}", v.major, v.minor, v.patch);

        // [A] _decimal — the single-phase-init C-ext that segfaulted ADR-010's subinterpreters.
        let decimal = py.import("decimal")?;
        let dec = decimal.getattr("Decimal")?;
        let sum = dec.call1(("1.1",))?.call_method1("__add__", (dec.call1(("2.2",))?,))?;
        println!("[A] _decimal in-process: Decimal('1.1') + Decimal('2.2') = {sum}  (no crash)");

        // [B] drive riptide's executor by FFI: define a module with test_*, call each, map outcome.
        let code = c"
def test_addition():
    assert 1 + 1 == 2

def test_intentional_fail():
    assert 2 + 2 == 5

def test_upper():
    assert 'ab'.upper() == 'AB'
";
        let module = PyModule::from_code(py, code, c"probe_tests.py", c"probe_tests")?;
        let mut results = Vec::new();
        for name in ["test_addition", "test_intentional_fail", "test_upper"] {
            let outcome = match module.getattr(name)?.call0() {
                Ok(_) => "passed",
                Err(e) if e.is_instance_of::<PyAssertionError>(py) => "failed",
                Err(_) => "error",
            };
            results.push((name, outcome));
            println!("[B] {name:<22} {outcome}");
        }

        let go = results
            == [
                ("test_addition", "passed"),
                ("test_intentional_fail", "failed"),
                ("test_upper", "passed"),
            ];
        println!("\n=== VERDICT: {} ===", if go { "GO — PyO3 embeds + FFI-drives riptide's executor" } else { "NO-GO" });
        Ok(())
    })
}
