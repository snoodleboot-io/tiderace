//! `riptide-daemon` — the warm test server's runnable front-end (design 08, ADR-E007). Thin glue that
//! composes the (unit-tested + e2e-proven) library pieces; the binary itself adds no logic.
//!
//! Modes:
//!   - `run <root>`   — one-shot: discover + run all through a warm wellspring, print a report.
//!   - `serve <root>` — bind the per-project Unix socket and serve RPC clients until Shutdown (unix).
//!   - `watch <root>` — block, and on each save re-run only the impacted tests (the inner loop).
//!
//! Env: `RIPTIDE_SHIM` (path to `py-shim/shim.py`, required); `RIPTIDE_PYTHON` (default `python3`);
//! `RIPTIDE_SOCKET` (serve mode socket path; default `<tmp>/riptide-daemon.sock`).

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use engine_core::cache::LocalCache;
use engine_core::coverage::DepGraph;
use engine_core::domain::NodeId;
use engine_daemon::{EngineHandler, RpcHandler, RpcRequest, RpcResponse, Session};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: riptide-daemon <run|serve|watch> <root>");
        return ExitCode::from(64);
    }
    let mode = args[1].as_str();
    let root = PathBuf::from(&args[2]);

    let python = std::env::var("RIPTIDE_PYTHON").unwrap_or_else(|_| "python3".to_string());
    let shim = match std::env::var("RIPTIDE_SHIM") {
        Ok(s) => PathBuf::from(s),
        Err(_) => {
            eprintln!("error: set RIPTIDE_SHIM to the path of py-shim/shim.py");
            return ExitCode::FAILURE;
        }
    };
    let mut handler = EngineHandler::new(python, shim, root.clone());

    match mode {
        "run" => cmd_run(&mut handler),
        "watch" => cmd_watch(&root, &mut handler),
        "serve" => cmd_serve(&mut handler),
        other => {
            eprintln!("unknown mode: {other}");
            ExitCode::from(64)
        }
    }
}

fn cmd_run(handler: &mut EngineHandler) -> ExitCode {
    match handler.handle(RpcRequest::Run { node_ids: vec![] }) {
        RpcResponse::Ran { results } => {
            let mut failures = 0;
            for r in &results {
                if r.outcome == "failed" || r.outcome == "error" {
                    failures += 1;
                }
                println!("{}\t{}", r.outcome.to_uppercase(), r.node_id);
            }
            eprintln!("{} tests, {} failing", results.len(), failures);
            if failures == 0 {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        RpcResponse::Error { message } => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
        _ => ExitCode::FAILURE,
    }
}

fn cmd_watch(root: &Path, handler: &mut EngineHandler) -> ExitCode {
    // Cold start: collect the candidate node set; the DepGraph is empty until coverage runs accrue,
    // so the first edits conservatively re-run all (correct), tightening as coverage populates it.
    let candidates: Vec<NodeId> = match handler.handle(RpcRequest::Discover) {
        RpcResponse::Discovered { node_ids } => node_ids.into_iter().map(NodeId::new).collect(),
        RpcResponse::Error { message } => {
            eprintln!("error: {message}");
            return ExitCode::FAILURE;
        }
        _ => Vec::new(),
    };
    let mut session = Session::new(
        DepGraph::new(),
        LocalCache::new(),
        candidates,
        env!("CARGO_PKG_VERSION"),
        "python",
        std::env::consts::OS,
    );
    eprintln!("watching {} (Ctrl-C to stop)…", root.display());
    let result = engine_daemon::watch_loop(
        root,
        &mut session,
        handler,
        Duration::from_millis(50),
        |path, action| println!("{}: {:?}", path.display(), action),
    );
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("watch error: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(unix)]
fn cmd_serve(handler: &mut EngineHandler) -> ExitCode {
    let path = std::env::var("RIPTIDE_SOCKET")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir().join("riptide-daemon.sock"));
    eprintln!("serving on {} …", path.display());
    match engine_daemon::serve_unix_socket(&path, handler) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("serve error: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(not(unix))]
fn cmd_serve(_handler: &mut EngineHandler) -> ExitCode {
    eprintln!("serve: the Unix-socket server is not available on this platform; use `run`/`watch`");
    ExitCode::from(64)
}
