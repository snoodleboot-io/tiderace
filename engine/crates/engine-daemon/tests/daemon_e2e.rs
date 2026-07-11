//! End-to-end daemon test (Phase 6): drive the RPC connection loop with the **live** `EngineHandler`
//! over a real wellspring, against a temp corpus — Discover, Run (twice, to prove warmth), Health,
//! Shutdown — asserting real per-test outcomes.
//!
//! Gated on the Phase-3 venv + shim being present (the same guard engine-core's live tests use), so it
//! runs here and skips cleanly in environments without Python.

use std::io::{self, Cursor, Read, Write};
use std::path::PathBuf;

use engine_daemon::{
    read_frame, serve_connection, write_frame, EngineHandler, RpcRequest, RpcResponse,
};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repo root")
}

fn venv_python() -> Option<PathBuf> {
    let p = repo_root().join(".riptide-fx-venv/bin/python");
    p.exists().then_some(p)
}

fn shim() -> PathBuf {
    repo_root().join("engine/py-shim/shim.py")
}

/// In-memory bidirectional stream: serves preloaded request frames, captures written responses.
struct Duplex {
    inbox: Cursor<Vec<u8>>,
    outbox: Vec<u8>,
}
impl Read for Duplex {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.inbox.read(buf)
    }
}
impl Write for Duplex {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.outbox.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn framed(reqs: &[RpcRequest]) -> Vec<u8> {
    let mut buf = Vec::new();
    for r in reqs {
        write_frame(&mut buf, r).unwrap();
    }
    buf
}

fn responses(bytes: &[u8]) -> Vec<RpcResponse> {
    let mut cur = Cursor::new(bytes.to_vec());
    let mut out = Vec::new();
    while let Some(r) = read_frame::<_, RpcResponse>(&mut cur).unwrap() {
        out.push(r);
    }
    out
}

#[test]
fn daemon_discovers_runs_and_stays_warm_over_a_real_wellspring() {
    let Some(python) = venv_python() else {
        eprintln!("skipping: .riptide-fx-venv not present");
        return;
    };

    // A temp corpus: one passing, one failing test (plain pytest-style — no imports needed).
    let dir = std::env::temp_dir().join(format!("riptide_daemon_e2e_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("test_sample.py"),
        "def test_pass():\n    assert 1 + 1 == 2\n\ndef test_fail():\n    assert 1 == 2\n",
    )
    .unwrap();

    let mut handler = EngineHandler::new(python.to_string_lossy().to_string(), shim(), dir.clone());

    let mut stream = Duplex {
        inbox: Cursor::new(framed(&[
            RpcRequest::Discover,
            RpcRequest::Run { node_ids: vec![] }, // all
            RpcRequest::Health,                   // warm after the first Run
            RpcRequest::Run {
                node_ids: vec![format!("test_sample.py::test_pass")],
            }, // reuse the wellspring
            RpcRequest::Shutdown,
        ])),
        outbox: Vec::new(),
    };

    let shutdown = serve_connection(&mut stream, &mut handler).unwrap();
    let resps = responses(&stream.outbox);
    let _ = std::fs::remove_dir_all(&dir);

    assert!(shutdown, "Shutdown ends the loop");
    assert_eq!(resps.len(), 5);

    // Discover found both tests.
    match &resps[0] {
        RpcResponse::Discovered { node_ids } => {
            assert_eq!(node_ids.len(), 2, "discovered both tests: {node_ids:?}");
        }
        other => panic!("expected Discovered, got {other:?}"),
    }

    // First Run executed both with the right outcomes through the real engine.
    match &resps[1] {
        RpcResponse::Ran { results } => {
            assert_eq!(results.len(), 2);
            let outcome = |n: &str| {
                results
                    .iter()
                    .find(|r| r.node_id.ends_with(n))
                    .map(|r| r.outcome.as_str())
            };
            assert_eq!(outcome("test_pass"), Some("passed"));
            assert_eq!(outcome("test_fail"), Some("failed"));
        }
        other => panic!("expected Ran, got {other:?}"),
    }

    // Health: the wellspring is warm (launched on the first Run, reused since).
    match &resps[2] {
        RpcResponse::Healthy { pid, warm } => {
            assert!(*warm, "daemon is warm after the first run");
            assert!(*pid > 0, "a real wellspring pid");
        }
        other => panic!("expected Healthy, got {other:?}"),
    }

    // Second Run (a single selected test) reuses the warm wellspring.
    match &resps[3] {
        RpcResponse::Ran { results } => {
            assert_eq!(results.len(), 1);
            assert_eq!(results[0].outcome, "passed");
        }
        other => panic!("expected Ran, got {other:?}"),
    }

    assert!(matches!(resps[4], RpcResponse::ShuttingDown));
}
