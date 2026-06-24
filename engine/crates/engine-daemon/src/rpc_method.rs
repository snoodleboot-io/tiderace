use serde::{Deserialize, Serialize};

/// A request from a thin client (CLI or IDE) to the warm daemon (design 08, ADR-E007). JSON over the
/// per-project local socket; the daemon is the single source of truth for warm state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "method", content = "params", rename_all = "snake_case")]
pub enum RpcRequest {
    /// List the currently-collected test node ids.
    Discover,
    /// Run a specific set of tests (empty ⇒ all collected).
    Run { node_ids: Vec<String> },
    /// Start watching; the daemon streams impacted re-runs until cancelled.
    Watch,
    /// Drop warm state (a stale interpreter after a conftest/config/C-ext change) and re-run all.
    Recycle,
    /// Liveness/warmth probe.
    Health,
    /// Ask the daemon to exit.
    Shutdown,
}

/// One test's result as carried over RPC.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RpcResult {
    pub node_id: String,
    pub outcome: String,
    pub duration_ms: u64,
}

/// The daemon's reply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", content = "data", rename_all = "snake_case")]
pub enum RpcResponse {
    Discovered { node_ids: Vec<String> },
    Ran { results: Vec<RpcResult> },
    Watching,
    Healthy { pid: i64, warm: bool },
    ShuttingDown,
    Error { message: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_roundtrips_through_json() {
        for req in [
            RpcRequest::Discover,
            RpcRequest::Run {
                node_ids: vec!["t.py::a".into()],
            },
            RpcRequest::Watch,
            RpcRequest::Recycle,
            RpcRequest::Health,
            RpcRequest::Shutdown,
        ] {
            let s = serde_json::to_string(&req).unwrap();
            assert_eq!(serde_json::from_str::<RpcRequest>(&s).unwrap(), req);
        }
    }

    #[test]
    fn response_roundtrips_and_is_tagged() {
        let resp = RpcResponse::Ran {
            results: vec![RpcResult {
                node_id: "t.py::a".into(),
                outcome: "passed".into(),
                duration_ms: 3,
            }],
        };
        let s = serde_json::to_string(&resp).unwrap();
        assert!(s.contains("\"status\":\"ran\""));
        assert_eq!(serde_json::from_str::<RpcResponse>(&s).unwrap(), resp);
    }
}
