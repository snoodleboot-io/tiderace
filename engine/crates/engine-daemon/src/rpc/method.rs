use engine_core::domain::{Outcome, TestResult};
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
    /// The full parallel run — what `tiderace run` does — served from the daemon's warm image
    /// (TID-84). Answered with [`RpcResponse::RanFull`]: everything a report needs per node.
    /// `keyword` / `marker` / `strict_markers` are the run's `-k` / `-m` / `--strict-markers`,
    /// applied by the workers forked off the image (TID-90); absent, the image's own selection
    /// (the project's `addopts`) stands.
    RunFull {
        #[serde(default)]
        keyword: Option<String>,
        #[serde(default)]
        marker: Option<String>,
        #[serde(default)]
        strict_markers: bool,
    },
    /// Start watching; the daemon streams impacted re-runs until cancelled.
    Watch,
    /// Drop warm state (a stale interpreter after a conftest/config/C-ext change) and re-run all.
    Recycle,
    /// Liveness/warmth probe.
    Health,
    /// Ask the daemon to exit.
    Shutdown,
}

impl RpcRequest {
    /// The unfiltered full run.
    pub fn run_full_all() -> Self {
        RpcRequest::RunFull {
            keyword: None,
            marker: None,
            strict_markers: false,
        }
    }
}

/// One test's result as carried over RPC.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RpcResult {
    pub node_id: String,
    pub outcome: Outcome,
    pub duration_ms: u64,
}

/// The daemon's reply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", content = "data", rename_all = "snake_case")]
pub enum RpcResponse {
    Discovered {
        node_ids: Vec<String>,
    },
    Ran {
        results: Vec<RpcResult>,
    },
    /// The `TestResult`s the CLI would have produced itself, so its report, exit code and
    /// schedule timeline are the same either way (TID-84).
    RanFull {
        results: Vec<TestResult>,
    },
    Watching,
    Healthy {
        pid: u32,
        warm: bool,
    },
    ShuttingDown,
    Error {
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_roundtrips_through_json() {
        for req in [
            RpcRequest::Discover,
            RpcRequest::run_full_all(),
            RpcRequest::RunFull {
                keyword: Some("not slow".into()),
                marker: None,
                strict_markers: true,
            },
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
                outcome: Outcome::Passed,
                duration_ms: 3,
            }],
        };
        let s = serde_json::to_string(&resp).unwrap();
        assert!(s.contains("\"status\":\"ran\""));
        assert_eq!(serde_json::from_str::<RpcResponse>(&s).unwrap(), resp);
    }
}
