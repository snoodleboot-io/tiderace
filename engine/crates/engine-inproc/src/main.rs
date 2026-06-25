//! ② backend proof: drive a real corpus through [`InProcessTransport`] — one embedded interpreter,
//! imported once, fork-from-embedded per test, results returned by FFI as `ExecResponse` Rust values
//! (no subprocess, no pipe/JSON control plane). **No pytest.**

use std::path::PathBuf;

use engine_core::exec::{ExecRequest, ShimTransport};
use engine_inproc::{engine_py_paths, InProcessTransport};

fn main() {
    let dir = std::env::temp_dir().join(format!("inproc_probe_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("test_x.py"),
        "def test_ok():\n    assert 1 + 1 == 2\n\
         def test_bad():\n    assert 1 == 2\n\
         def test_upper():\n    assert 'ab'.upper() == 'AB'\n",
    )
    .unwrap();

    let engine_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();

    let mut transport = InProcessTransport::new(&dir, engine_py_paths(&engine_dir), false);
    let ready = transport.ready().expect("ready");
    println!(
        "[ready] in-process wellspring pid={} (one embedded interpreter, project imported once)",
        ready.pid
    );

    let expected = [
        ("test_ok", "passed"),
        ("test_bad", "failed"),
        ("test_upper", "passed"),
    ];
    let mut ok = true;
    for (name, want) in expected {
        let node = format!("test_x.py::{name}");
        let resp = transport
            .exchange(&ExecRequest::bare(&node, "pytest_func", 5000))
            .expect("exchange");
        let mark = if resp.outcome == want {
            "ok"
        } else {
            ok = false;
            "!!"
        };
        println!("[run] {name:<12} {:<8} {mark}", resp.outcome);
    }

    println!(
        "\n=== VERDICT: {} ===",
        if ok {
            "GO — InProcessTransport drives tests by FFI (fork-from-embedded, no subprocess/pipe)"
        } else {
            "NO-GO"
        }
    );
    let _ = std::fs::remove_dir_all(&dir);
    std::process::exit(if ok { 0 } else { 1 });
}
