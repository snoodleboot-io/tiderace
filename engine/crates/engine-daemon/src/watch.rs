use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

use engine_core::cache::Cache;
use engine_core::fixtures::ClosureHasher;

use crate::fs_watcher::{Debouncer, FsWatcher};
use crate::rpc_method::{RpcRequest, RpcResponse};
use crate::rpc_server::RpcHandler;
use crate::session::{ChangeOutcome, Session};

/// What `tiderace watch` did in response to one edit (the visible inner-loop result).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchAction {
    /// Content-identical / irrelevant file — nothing ran.
    Idle,
    /// `n` impacted-and-uncached tests re-ran.
    Ran(usize),
    /// A test file changed → re-collected and re-ran (`n` tests).
    Recollected(usize),
    /// A conftest/config/C-ext changed → warm interpreter recycled, then re-ran (`n` tests).
    Recycled(usize),
}

/// The 32-byte content digest the [`Session`]/cache key consume, reusing the engine's deterministic
/// [`ClosureHasher`] (no extra hash dependency).
pub fn content_hash(bytes: &[u8]) -> [u8; 32] {
    *ClosureHasher::new().feed(bytes).finish().as_bytes()
}

/// React to one changed file — the edit→result core of `tiderace watch`. Classifies the change via the
/// [`Session`] (content-hash invalidation → impact selection → cache filtering), then drives the
/// `handler` to do the *minimum* work: re-run only impacted+uncached tests, re-collect a changed test
/// file, or recycle the warm interpreter on a conftest/config/C-ext change. Returns what happened.
pub fn react_to_change<C: Cache>(
    session: &mut Session<C>,
    handler: &mut dyn RpcHandler,
    path: &Path,
    content: &[u8],
    changed_lines: Option<BTreeSet<u32>>,
) -> WatchAction {
    let hash = content_hash(content);
    match session.on_change(path, hash, changed_lines) {
        ChangeOutcome::Nothing => WatchAction::Idle,
        ChangeOutcome::Rerun(nodes) => {
            if nodes.is_empty() {
                return WatchAction::Ran(0); // impacted-but-all-cached ⇒ no execution
            }
            let node_ids = nodes.iter().map(|n| n.as_str().to_string()).collect();
            WatchAction::Ran(ran_count(handler.handle(RpcRequest::Run { node_ids })))
        }
        ChangeOutcome::Recollect(_) => WatchAction::Recollected(ran_count(
            handler.handle(RpcRequest::Run { node_ids: vec![] }),
        )),
        ChangeOutcome::Recycle(_) => {
            WatchAction::Recycled(ran_count(handler.handle(RpcRequest::Recycle)))
        }
    }
}

fn ran_count(resp: RpcResponse) -> usize {
    match resp {
        RpcResponse::Ran { results } => results.len(),
        _ => 0,
    }
}

/// The blocking `tiderace watch` loop: watch `root`, coalesce each save's event burst within a quiet
/// window, and run [`react_to_change`] per changed file, reporting via `on_action`. Runs until the
/// watcher channel closes (Ctrl-C). Thin integration over the unit-tested pieces (FsWatcher debounce,
/// Session classification, handler dispatch) — the same shape as [`serve_unix_socket`](crate::serve_unix_socket).
pub fn watch_loop<C: Cache>(
    root: &Path,
    session: &mut Session<C>,
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
            if let Ok(content) = std::fs::read(&path) {
                let action = react_to_change(session, handler, &path, &content, None);
                on_action(&path, &action);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use engine_core::cache::LocalCache;
    use engine_core::coverage::{CoverageReport, DepGraph};
    use engine_core::domain::NodeId;

    use super::*;

    /// Records the Run/Recycle requests it gets and answers with `running` results each time.
    struct FakeHandler {
        running: usize,
        seen: Vec<String>,
    }
    impl RpcHandler for FakeHandler {
        fn handle(&mut self, request: RpcRequest) -> RpcResponse {
            use crate::rpc_method::RpcResult;
            let tag = match &request {
                RpcRequest::Run { node_ids } => format!("run:{}", node_ids.len()),
                RpcRequest::Recycle => "recycle".to_string(),
                _ => "other".to_string(),
            };
            self.seen.push(tag);
            RpcResponse::Ran {
                results: (0..self.running)
                    .map(|i| RpcResult {
                        node_id: format!("n{i}"),
                        outcome: "passed".into(),
                        duration_ms: 1,
                    })
                    .collect(),
            }
        }
    }

    fn session() -> Session<LocalCache> {
        let mut graph = DepGraph::new();
        let mut wire = BTreeMap::new();
        wire.insert("src.py".to_string(), vec![1u32, 2]);
        graph.record(CoverageReport::from_wire(NodeId::new("test_m.py::a"), wire));
        let mut s = Session::new(
            graph,
            LocalCache::new(),
            vec![NodeId::new("test_m.py::a")],
            "0.5",
            "3.12",
            "linux",
        );
        s.seed_hash("src.py", content_hash(b"old source"));
        s
    }

    #[test]
    fn source_edit_reruns_impacted() {
        let mut s = session();
        let mut h = FakeHandler {
            running: 1,
            seen: vec![],
        };
        let action = react_to_change(
            &mut s,
            &mut h,
            Path::new("src.py"),
            b"new source",
            Some([1].into()),
        );
        assert_eq!(action, WatchAction::Ran(1));
        assert_eq!(h.seen, vec!["run:1"]); // ran exactly the impacted node
    }

    #[test]
    fn mtime_only_touch_is_idle() {
        let mut s = session();
        let mut h = FakeHandler {
            running: 9,
            seen: vec![],
        };
        // Same content as seeded ⇒ Nothing ⇒ no handler call.
        let action = react_to_change(&mut s, &mut h, Path::new("src.py"), b"old source", None);
        assert_eq!(action, WatchAction::Idle);
        assert!(h.seen.is_empty());
    }

    #[test]
    fn conftest_change_recycles() {
        let mut s = session();
        let mut h = FakeHandler {
            running: 1,
            seen: vec![],
        };
        let action = react_to_change(&mut s, &mut h, Path::new("conftest.py"), b"x", None);
        assert_eq!(action, WatchAction::Recycled(1));
        assert_eq!(h.seen, vec!["recycle"]);
    }

    #[test]
    fn test_file_change_recollects() {
        let mut s = session();
        let mut h = FakeHandler {
            running: 2,
            seen: vec![],
        };
        let action = react_to_change(&mut s, &mut h, Path::new("test_m.py"), b"changed", None);
        assert_eq!(action, WatchAction::Recollected(2));
        assert_eq!(h.seen, vec!["run:0"]); // re-collect = Run all (empty selector)
    }
}
