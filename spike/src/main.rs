//! Phase-1 Wellspring spike orchestrator.
//!
//! Drives the Python `shim.py` Wellspring over a length-prefixed (u32 LE) JSON frame protocol
//! and reports per-test outcomes. Two modes:
//!
//! - `warm`: one Wellspring imports the corpus once, then forks a pristine child per test.
//! - `fresh`: a brand-new `python` process per test (no warm reuse) — the comparison baseline.
//!
//! This is spike code: real and exercised, but explicitly slated for productionization in
//! Phase 2 (it is not a stub). Outcome lines are printed as `<node_id>\t<outcome>`.

use std::io::{self, Read, Write};
use std::process::{Command, Stdio};

/// A test to run: `(style, node_id)` where style is `pytest_func` or `unittest_method`.
struct Spec {
    style: String,
    node_id: String,
}

fn parse_specs(args: &[String]) -> Vec<Spec> {
    args.iter()
        .map(|s| {
            let (style, node_id) = s.split_once(':').expect("spec must be 'style:node_id'");
            Spec {
                style: style.to_string(),
                node_id: node_id.to_string(),
            }
        })
        .collect()
}

fn write_frame<W: Write>(w: &mut W, v: &serde_json::Value) -> io::Result<()> {
    let bytes = serde_json::to_vec(v)?;
    w.write_all(&(bytes.len() as u32).to_le_bytes())?;
    w.write_all(&bytes)?;
    w.flush()
}

fn read_frame<R: Read>(r: &mut R) -> io::Result<Option<serde_json::Value>> {
    let mut header = [0u8; 4];
    if let Err(e) = r.read_exact(&mut header) {
        if e.kind() == io::ErrorKind::UnexpectedEof {
            return Ok(None);
        }
        return Err(e);
    }
    let len = u32::from_le_bytes(header) as usize;
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    Ok(Some(serde_json::from_slice(&buf)?))
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

/// Warm path: a single Wellspring forks per test.
fn run_warm(corpus: &str, specs: &[Spec]) -> io::Result<()> {
    let python = env_or("SPIKE_PYTHON", "python3");
    let shim = env_or("SPIKE_SHIM", "shim.py");

    let mut child = Command::new(&python)
        .arg(&shim)
        .arg(corpus)
        // Pin BLAS/OMP thread pools to 1 — threaded native pools + fork() are a known hazard;
        // this is the documented fork-safety mitigation (a Phase-3 reinit/thread-policy learning).
        .env("OPENBLAS_NUM_THREADS", "1")
        .env("OMP_NUM_THREADS", "1")
        .env("MKL_NUM_THREADS", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;

    let mut cin = child.stdin.take().expect("stdin");
    let mut cout = child.stdout.take().expect("stdout");

    // Startup handshake.
    let ready = read_frame(&mut cout)?.expect("wellspring sent no ready frame");
    if ready.get("ready").and_then(|v| v.as_bool()) != Some(true) {
        eprintln!("wellspring failed to warm: {ready}");
        std::process::exit(2);
    }

    for spec in specs {
        let req = serde_json::json!({
            "node_id": spec.node_id,
            "style": spec.style,
            "deadline_ms": 5000,
        });
        write_frame(&mut cin, &req)?;
        let resp = read_frame(&mut cout)?.expect("wellspring closed mid-run");
        let outcome = resp
            .get("outcome")
            .and_then(|v| v.as_str())
            .unwrap_or("error");
        println!("{}\t{}", spec.node_id, outcome);
    }

    drop(cin); // EOF -> shim exits
    child.wait()?;
    Ok(())
}

/// Fresh-process runner inlined into a one-shot `python -c`. Imports numpy + the module fresh
/// every time (no warm reuse), so it measures the cold per-test startup the Wellspring avoids.
const FRESH_RUNNER: &str = r#"
import sys, importlib, unittest, traceback
corpus, node_id, style = sys.argv[1], sys.argv[2], sys.argv[3]
sys.path.insert(0, corpus)
import numpy  # paid per-test in the cold baseline
path, _, rest = node_id.partition("::")
mod = importlib.import_module(path[:-3].replace("/", ".") if path.endswith(".py") else path.replace("/", "."))
try:
    if style == "unittest_method":
        cls_name, _, method = rest.partition("::")
        r = unittest.TestResult(); getattr(mod, cls_name)(method).run(r)
        out = "error" if r.errors else "failed" if r.failures else "skipped" if r.skipped else "passed"
    else:
        getattr(mod, rest)(); out = "passed"
except AssertionError:
    out = "failed"
except Exception:
    out = "error"
print(out)
"#;

fn run_fresh(corpus: &str, specs: &[Spec]) -> io::Result<()> {
    let python = env_or("SPIKE_PYTHON", "python3");
    for spec in specs {
        let output = Command::new(&python)
            .arg("-c")
            .arg(FRESH_RUNNER)
            .arg(corpus)
            .arg(&spec.node_id)
            .arg(&spec.style)
            .output()?;
        let outcome = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let outcome = if outcome.is_empty() {
            "error".to_string()
        } else {
            outcome
        };
        println!("{}\t{}", spec.node_id, outcome);
    }
    Ok(())
}

fn main() -> io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: spike <warm|fresh> <corpus_dir> <style:node_id>...");
        std::process::exit(64);
    }
    let mode = &args[1];
    let corpus = &args[2];
    let specs = parse_specs(&args[3..]);
    match mode.as_str() {
        "warm" => run_warm(corpus, &specs),
        "fresh" => run_fresh(corpus, &specs),
        other => {
            eprintln!("unknown mode: {other}");
            std::process::exit(64);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_roundtrips() {
        let value = serde_json::json!({"node_id": "m::t", "outcome": "passed"});
        let mut buf = Vec::new();
        write_frame(&mut buf, &value).unwrap();
        // header is the LE length of the JSON payload
        let declared = u32::from_le_bytes(buf[..4].try_into().unwrap()) as usize;
        assert_eq!(declared, buf.len() - 4);
        let mut cursor = io::Cursor::new(buf);
        let back = read_frame(&mut cursor).unwrap().unwrap();
        assert_eq!(back, value);
    }

    #[test]
    fn read_frame_on_empty_is_none() {
        let mut empty = io::Cursor::new(Vec::new());
        assert!(read_frame(&mut empty).unwrap().is_none());
    }

    #[test]
    fn parse_specs_splits_on_first_colon() {
        let specs = parse_specs(&["pytest_func:test_basic::test_x".to_string()]);
        assert_eq!(specs[0].style, "pytest_func");
        assert_eq!(specs[0].node_id, "test_basic::test_x");
    }
}
