//! TID-96 — a recorded state-disturber runs in the module's one forked child, not a fork per test.
//!
//! A unittest class whose `setUpClass` starts a server thread leaves that thread behind, so every
//! test in it is recorded `must_fork`. The next run denied those nodes the in-process tier and the
//! shim forked each one separately — a fresh process each time, so `setUpClass` ran once per
//! method (pirn-agents' replay class paid its 5s set-up seven times). A recorded disturber now
//! takes the opaque-module route: one child per module, tests in file order, `setUpClass` once.

#![cfg(unix)]

use engine_core::testing::{python, shim, skip_live, PythonNeeds};
use engine_daemon::EngineHandler;
use std::path::Path;

fn setup_calls(dir: &Path) -> usize {
    std::fs::read_to_string(dir.join("setup_calls.txt"))
        .map(|s| s.lines().count())
        .unwrap_or(0)
}

#[test]
fn a_recorded_disturbers_class_setup_runs_once_per_module_child() {
    let Some(python) = python(PythonNeeds::Any) else {
        skip_live("no Python interpreter available");
        return;
    };
    let dir = std::env::temp_dir().join(format!("tiderace_t96_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("test_server_class.py"),
        "import os, threading, unittest, http.server, socketserver\n\n\
         COUNTS = os.path.join(os.path.dirname(__file__), 'setup_calls.txt')\n\n\
         class Handler(http.server.BaseHTTPRequestHandler):\n    \
             def do_GET(self):\n        \
                 self.send_response(200); self.end_headers(); self.wfile.write(b'ok')\n    \
             def log_message(self, *a):\n        pass\n\n\
         class TestServerClass(unittest.TestCase):\n    \
             @classmethod\n    \
             def setUpClass(cls):\n        \
                 with open(COUNTS, 'a') as f:\n            f.write('setUpClass\\n')\n        \
                 cls._server = socketserver.TCPServer(('127.0.0.1', 0), Handler)\n        \
                 cls._thread = threading.Thread(target=cls._server.serve_forever, daemon=True)\n        \
                 cls._thread.start()\n\n    \
             @classmethod\n    \
             def tearDownClass(cls):\n        cls._server.shutdown()\n\n    \
             def test_a(self):\n        self.assertTrue(self._server)\n\n    \
             def test_b(self):\n        self.assertTrue(self._thread.is_alive())\n\n    \
             def test_c(self):\n        self.assertTrue(True)\n",
    )
    .unwrap();
    let mut handler = EngineHandler::new(python, shim(), dir.clone());

    // Run 1: the first test leaves the server thread behind; the class is recorded as a disturber.
    let first = handler.run_full_parallel().expect("first run");
    assert_eq!(first.len(), 3, "{first:?}");
    assert!(first.iter().all(|r| r.outcome == "passed"), "{first:?}");
    let after_first = setup_calls(&dir);
    assert!(after_first >= 1, "setUpClass ran in run 1");
    let state: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join(".tiderace-state.json")).unwrap())
            .unwrap();
    let recorded: Vec<bool> = state["tests"]
        .as_object()
        .unwrap()
        .values()
        .map(|r| r["must_fork"].as_bool().unwrap_or(false))
        .collect();
    assert!(
        recorded.iter().any(|m| *m),
        "the disturber is recorded: {state}"
    );

    // Run 2: every test is a recorded disturber — one module child, setUpClass once, not thrice.
    let _ = std::fs::remove_file(dir.join("setup_calls.txt"));
    let second = handler.run_full_parallel().expect("second run");
    assert_eq!(second.len(), 3, "{second:?}");
    assert!(second.iter().all(|r| r.outcome == "passed"), "{second:?}");
    assert_eq!(
        setup_calls(&dir),
        1,
        "setUpClass once for the class in its module child, not once per forked test"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
