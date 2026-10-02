//! `tiderace-daemon watch`: the edit→result inner loop, on the daemon's own run path.
//!
//! Each save is classified by what it can have invalidated, and the warm handler does the rest.
//! A change to `conftest.py`, a project config file, `setup.py` or a C extension stales the
//! imported image, so the handler recycles its warm interpreter and runs everything; any other
//! `.py` change goes through the daemon's full run — `RpcRequest::RunFull`, the same
//! `state::plan` impact selection `tiderace run` uses, which re-collects, re-runs what the saved
//! file reaches and serves the rest from the record. Anything else is noise.
//!
//! This replaces a second impact pipeline (`Session` / `Invalidator` over a `DepGraph` that was
//! never populated) that used to live beside the real one (TID-110).
use std::path::Path;
use std::time::Duration;

use crate::fs_watcher::{Debouncer, FsWatcher};
use crate::rpc::method::{RpcRequest, RpcResponse};
use crate::rpc::server::RpcHandler;

/// What `tiderace watch` did in response to one edit (the visible inner-loop result).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchAction {
    /// Not a Python source, config or extension — nothing ran.
    Idle,
    /// The daemon's run: `n` tests ran (the impacted ones, or all of them when nothing vouches).
    Ran(usize),
    /// A conftest / config / C-extension changed → warm interpreter recycled, then all re-ran (`n`).
    Recycled(usize),
}

/// Beyond pytest's config files, the two that shape the imported image itself.
const RECYCLE_ALSO: &[&str] = &["setup.py", "conftest.py"];

enum Kind {
    Recycle,
    Source,
    Ignored,
}

fn classify(path: &Path) -> Kind {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if engine_core::collection::CONFIG_FILES.contains(&name) || RECYCLE_ALSO.contains(&name) {
        return Kind::Recycle;
    }
    match path.extension().and_then(|e| e.to_str()) {
        Some("so") | Some("pyd") | Some("dylib") => Kind::Recycle, // C-ext: in-interpreter state stale
        Some("py") => Kind::Source,
        _ => Kind::Ignored,
    }
}

/// React to one changed file — the edit→result core of `tiderace watch`.
pub fn react_to_change(handler: &mut dyn RpcHandler, path: &Path) -> WatchAction {
    match classify(path) {
        Kind::Ignored => WatchAction::Idle,
        Kind::Recycle => WatchAction::Recycled(ran_count(handler.handle(RpcRequest::Recycle))),
        Kind::Source => WatchAction::Ran(ran_count(handler.handle(RpcRequest::RunFull {
            keyword: None,
            marker: None,
            strict_markers: false,
        }))),
    }
}

fn ran_count(resp: RpcResponse) -> usize {
    match resp {
        RpcResponse::Ran { results } => results.len(),
        RpcResponse::RanFull { results } => results.len(),
        _ => 0,
    }
}

/// The blocking `tiderace watch` loop: watch `root`, coalesce each save's event burst within a quiet
/// window, and run [`react_to_change`] per changed file, reporting via `on_action`. Runs until the
/// watcher channel closes (Ctrl-C). Thin integration over the unit-tested pieces (FsWatcher debounce,
/// the classification, handler dispatch) — the same shape as [`serve_unix_socket`](crate::serve_unix_socket).
pub fn watch_loop(
    root: &Path,
    handler: &mut dyn RpcHandler,
    quiet_window: Duration,
    mut on_action: impl FnMut(&Path, &WatchAction),
) -> notify::Result<()> {
    let watcher = FsWatcher::watch(root)?;
    loop {
        // Block for the first event of a burst, then drain the rest within the quiet window.
        let Ok(first) = watcher.events().recv() else {
            return Ok(()); // channel closed ⇒ watcher dropped ⇒ stop
        };
        let mut debouncer = Debouncer::new();
        debouncer.record(first);
        while let Ok(path) = watcher.events().recv_timeout(quiet_window) {
            debouncer.record(path);
        }
        for path in debouncer.take() {
            let action = react_to_change(handler, &path);
            on_action(&path, &action);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpc::method::RpcResult;

    /// Records the requests it gets and answers each with `running` results.
    struct FakeHandler {
        running: usize,
        seen: Vec<String>,
    }
    impl RpcHandler for FakeHandler {
        fn handle(&mut self, request: RpcRequest) -> RpcResponse {
            let tag = match &request {
                RpcRequest::RunFull { .. } => "run-full".to_string(),
                RpcRequest::Recycle => "recycle".to_string(),
                _ => "other".to_string(),
            };
            self.seen.push(tag);
            let results: Vec<RpcResult> = (0..self.running)
                .map(|i| RpcResult {
                    node_id: format!("n{i}"),
                    outcome: engine_core::domain::Outcome::Passed,
                    duration_ms: 1,
                })
                .collect();
            match request {
                RpcRequest::RunFull { .. } => RpcResponse::RanFull {
                    results: results
                        .into_iter()
                        .map(|r| {
                            engine_core::domain::TestResult::new(
                                engine_core::domain::NodeId::new(r.node_id),
                                r.outcome,
                                r.duration_ms,
                                "",
                            )
                        })
                        .collect(),
                },
                _ => RpcResponse::Ran { results },
            }
        }
    }

    fn handler(running: usize) -> FakeHandler {
        FakeHandler {
            running,
            seen: vec![],
        }
    }

    #[test]
    fn a_source_edit_goes_through_the_daemons_run() {
        let mut h = handler(3);
        assert_eq!(
            react_to_change(&mut h, Path::new("src/auth.py")),
            WatchAction::Ran(3)
        );
        assert_eq!(
            react_to_change(&mut h, Path::new("tests/test_auth.py")),
            WatchAction::Ran(3)
        );
        assert_eq!(h.seen, vec!["run-full", "run-full"]);
    }

    #[test]
    fn a_conftest_config_or_extension_change_recycles() {
        for path in ["conftest.py", "pyproject.toml", "setup.py", "ext/fast.so"] {
            let mut h = handler(2);
            assert_eq!(
                react_to_change(&mut h, Path::new(path)),
                WatchAction::Recycled(2),
                "{path}"
            );
            assert_eq!(h.seen, vec!["recycle"]);
        }
    }

    #[test]
    fn anything_else_is_idle_and_touches_nothing() {
        let mut h = handler(9);
        assert_eq!(
            react_to_change(&mut h, Path::new("README.md")),
            WatchAction::Idle
        );
        assert_eq!(
            react_to_change(&mut h, Path::new("data.json")),
            WatchAction::Idle
        );
        assert!(h.seen.is_empty());
    }
}
