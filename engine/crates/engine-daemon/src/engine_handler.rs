use std::path::PathBuf;

use engine_core::collection::{Collector, RegexCollector};
use engine_core::domain::{Outcome, TestItem};
use engine_core::exec::{ForkWorker, Worker};

use crate::rpc_method::{RpcRequest, RpcResponse, RpcResult};
use crate::rpc_server::RpcHandler;

/// The live [`RpcHandler`]: turns RPC requests into real engine work over a **warm** wellspring
/// (design 08, ADR-E007). The `ForkWorker` is launched lazily on the first `Run` and **reused** across
/// requests, so the second run in a session pays no interpreter/import cost — the daemon's whole point.
pub struct EngineHandler {
    python: String,
    shim: PathBuf,
    root: PathBuf,
    worker: Option<ForkWorker>, // warm wellspring, kept alive across Run requests
}

impl EngineHandler {
    pub fn new(
        python: impl Into<String>,
        shim: impl Into<PathBuf>,
        root: impl Into<PathBuf>,
    ) -> Self {
        Self {
            python: python.into(),
            shim: shim.into(),
            root: root.into(),
            worker: None,
        }
    }

    fn collect(&self) -> Result<Vec<TestItem>, String> {
        RegexCollector::new()
            .collect(&self.root)
            .map_err(|e| format!("collection failed: {e}"))
    }

    /// Launch the wellspring once; reuse it thereafter (warm).
    fn worker(&mut self) -> Result<&mut ForkWorker, String> {
        if self.worker.is_none() {
            let w = ForkWorker::launch(&self.python, &self.shim, &self.root)
                .map_err(|e| format!("failed to launch wellspring: {e}"))?;
            self.worker = Some(w);
        }
        Ok(self.worker.as_mut().expect("just launched"))
    }

    fn run(&mut self, requested: &[String]) -> Result<Vec<RpcResult>, String> {
        let all = self.collect()?;
        let items: Vec<TestItem> = if requested.is_empty() {
            all
        } else {
            all.into_iter()
                .filter(|it| requested.iter().any(|r| r == it.node_id.as_str()))
                .collect()
        };
        let results = self
            .worker()?
            .run(&items)
            .map_err(|e| format!("execution failed: {e}"))?;
        Ok(results
            .into_iter()
            .map(|r| RpcResult {
                node_id: r.node_id.to_string(),
                outcome: outcome_token(r.outcome).to_string(),
                duration_ms: r.duration_ms,
            })
            .collect())
    }
}

impl RpcHandler for EngineHandler {
    fn handle(&mut self, request: RpcRequest) -> RpcResponse {
        match request {
            RpcRequest::Discover => match self.collect() {
                Ok(items) => RpcResponse::Discovered {
                    node_ids: items.iter().map(|i| i.node_id.to_string()).collect(),
                },
                Err(message) => RpcResponse::Error { message },
            },
            RpcRequest::Run { node_ids } => match self.run(&node_ids) {
                Ok(results) => RpcResponse::Ran { results },
                Err(message) => RpcResponse::Error { message },
            },
            RpcRequest::Recycle => {
                self.worker = None; // drop the stale warm interpreter; next Run relaunches it
                match self.run(&[]) {
                    Ok(results) => RpcResponse::Ran { results },
                    Err(message) => RpcResponse::Error { message },
                }
            }
            RpcRequest::Watch => RpcResponse::Watching,
            RpcRequest::Health => RpcResponse::Healthy {
                pid: self
                    .worker
                    .as_ref()
                    .map(ForkWorker::wellspring_pid)
                    .unwrap_or(-1),
                warm: self.worker.is_some(),
            },
            RpcRequest::Shutdown => RpcResponse::ShuttingDown,
        }
    }
}

fn outcome_token(outcome: Outcome) -> &'static str {
    match outcome {
        Outcome::Passed => "passed",
        Outcome::Failed => "failed",
        Outcome::Skipped => "skipped",
        Outcome::XFail => "xfail",
        Outcome::XPass => "xpass",
        Outcome::Error => "error",
    }
}
