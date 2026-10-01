use std::io::{self, Read, Write};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::domain::{NodeId, Outcome, TestStyle};
use crate::error::{EngineError, Result};
use crate::fixtures::{FixtureArgs, FixtureInstance};

/// A request to execute one test in a forked child of the Wellspring.
///
/// **Phase 3 extension (contract-frozen).** The Phase 2 wire fields (`node_id`, `style`,
/// `deadline_ms`) are unchanged. Phase 3 adds the fixture fields the forked child needs —
/// `post_fork` (Function-scope instances to set up in-child), `reinit` (fork-fragile resource node
/// ids to rebuild post-fork, W11), and `fixture_args` (the assembled argument map). All three are
/// `#[serde(skip_serializing_if = ...)]` so a **fixtureless** request serializes byte-identically to
/// the Phase 2 frame — the length-prefixed JSON framing itself is unchanged (Phase 2 CONTRACT §3).
#[derive(Debug, Serialize)]
pub struct ExecRequest<'a> {
    pub node_id: &'a NodeId,
    /// The test style; its serde form is the token the shim dispatches on.
    pub style: TestStyle,
    pub deadline_ms: u64,
    /// Function-scope fixture instances to set up in the forked child, topo order (design 05 §5.2).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub post_fork: Vec<FixtureInstance>,
    /// `reinit_after_fork` fixture node ids to rebuild in-child (W11).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reinit: Vec<String>,
    /// The assembled argument map the body is invoked with.
    #[serde(default, skip_serializing_if = "FixtureArgs::is_empty")]
    pub fixture_args: FixtureArgs,
    /// Ask the shim to run this test **in-process (no fork)** — the pure/restore fast path. The shim
    /// still forks if the module isn't snapshot-restorable (soundness). `false` ⇒ byte-identical frame.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub force_no_fork: bool,
    /// This test is a *recorded state-disturber* (TID-33): it must not take the in-process tier.
    /// Said explicitly so the shim can route it the way it routes an opaque module — one forked
    /// child per module, tests in file order, a class's `setUpClass` once — rather than a fork
    /// per test, which paid that set-up once per method (TID-96). `false` ⇒ byte-identical frame.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub must_fork: bool,
    /// This test is *recorded pure and unchanged* (TID-1): run it BARE no-fork — skip the snapshot/restore
    /// entirely. Only ever set for a `force_no_fork` request. `false` ⇒ byte-identical frame.
    ///
    /// Skipping the snapshot means skipping isolation, so any mutation this test makes persists. That is
    /// why it is gated on a recorded verdict whose dependencies are unchanged, and why the payoff — ~90×
    /// per test in the trivial-test microbenchmark, ~3.4× on a real snapshot-heavy corpus — only exists
    /// for tests that genuinely mutate nothing (TID-41).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub trusted_pure: bool,
}

impl<'a> ExecRequest<'a> {
    /// A Phase-2-shaped (fixtureless) request: the three wire fields, empty fixture fields. Keeps
    /// existing call sites concise and the frame byte-identical to Phase 2.
    pub fn bare(node_id: &'a NodeId, style: TestStyle, deadline_ms: u64) -> Self {
        Self {
            node_id,
            style,
            deadline_ms,
            post_fork: Vec::new(),
            reinit: Vec::new(),
            fixture_args: FixtureArgs::new(),
            force_no_fork: false,
            trusted_pure: false,
            must_fork: false,
        }
    }

    /// Ask the shim to run this test in-process (no fork) where sound — the fast path.
    pub fn no_fork(mut self) -> Self {
        self.force_no_fork = true;
        self
    }
}

/// The child's reported outcome for one test.
///
/// **Phase 5 extension (additive, Phase-3 CONTRACT §6).** `coverage` is the per-test executed-source
/// footprint (`relative_path -> sorted lines`) the shim captures under `--coverage` (ADR-E006); it is
/// `#[serde(default)]`, so a capture-off response is byte-identical to the Phase 2/3 frame and old
/// consumers ignore it. Fold it into a [`crate::coverage::CoverageReport`] via
/// [`crate::coverage::CoverageReport::from_wire`].
#[derive(Debug, Deserialize)]
pub struct ExecResponse {
    pub node_id: NodeId,
    /// The outcome; a token the engine does not know reads as [`Outcome::Error`].
    pub outcome: Outcome,
    #[serde(default)]
    pub detail: String,
    /// Per-test touched source: `relative_path -> sorted line numbers` (empty unless capture is on).
    #[serde(default)]
    pub coverage: std::collections::BTreeMap<String, Vec<u32>>,
    /// Purity verdict (TID-1): `Some(true)` measured pure, `Some(false)` measured impure, `None` not
    /// measured (forked / async / trusted-pure). Recordable verdicts drive the bare-no-fork fast path.
    #[serde(default)]
    pub pure: Option<bool>,
    /// One entry per parametrization case, when the node has any (TID-25).
    ///
    /// The shim already runs — and forks — each case separately, so these results are produced
    /// whether or not anyone reports them; collapsing to the node's worst outcome simply discarded
    /// work already done. Omitted entirely for an unparametrized node, which keeps its frame
    /// byte-identical to before.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub variants: Vec<VariantResult>,
    /// This node disturbed interpreter state the in-process ladder cannot model, so it must be
    /// forked from the start on later runs (TID-33).
    ///
    /// Deliberately separate from `pure`. Most impure tests are impure in ways restore handles
    /// completely — they mutate their own module's globals — and demoting those to forking would
    /// cost the ladder nearly everything it buys. This flag marks only the tests whose damage
    /// nothing undid.
    #[serde(default)]
    pub must_fork: bool,
    /// The module whose *import* skipped, when this skip covers a whole module (TID-55).
    ///
    /// A `pytest.importorskip` in a module or in the conftest above it skips every test it holds.
    /// pytest reports that as one skip; tiderace reports one per test, which is the more useful
    /// number and looks like a defect beside pytest's. Carrying the origin lets the summary say
    /// both — `578 skipped (12 modules skipped at import)`. Absent for a per-test skip (the
    /// shim spells that as `""`).
    #[serde(default, deserialize_with = "crate::domain::empty_as_none")]
    pub skip_origin: Option<String>,
    /// What `-k` matched this node against — path names, `::` segments, mark names — as the
    /// shim computed them (TID-102). Recorded by the daemon so it can take the verdict itself
    /// for a node whose dependencies are unchanged. Empty when the shim never reached the
    /// verdict (a module-import skip, a directory error).
    #[serde(default)]
    pub keywords: Vec<String>,
    /// `variants` is the complete answer for this request, even when empty (TID-26).
    ///
    /// Needed because an empty list is otherwise ambiguous: an unparametrized node also sends none.
    /// A class marked `inherited_methods` that turns out to inherit nothing must contribute zero
    /// results, not one.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub expanded: bool,
}

/// One parametrization case's result — a pytest-style `node_id[params]` and its own outcome.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct VariantResult {
    pub node_id: NodeId,
    pub outcome: Outcome,
    #[serde(default)]
    pub detail: String,
    #[serde(default)]
    pub duration_ms: u64,
    #[serde(default)]
    pub coverage: std::collections::BTreeMap<String, Vec<u32>>,
    #[serde(default)]
    pub pure: Option<bool>,
    /// See [`ExecResponse::keywords`] — this case's own, with its id as the last segment.
    #[serde(default)]
    pub keywords: Vec<String>,
    /// See [`ExecResponse::must_fork`] — recorded per case, since only some cases may trip.
    #[serde(default)]
    pub must_fork: bool,
}

/// The readiness frame, read: `{"ready": true, "pid": <int>}` from a shim that imported the
/// suite; anything else is a shim that did not warm, reported with the frame it sent instead.
pub(crate) fn ready_info(frame: serde_json::Value) -> Result<crate::exec::transport::ReadyInfo> {
    if frame.get("ready").and_then(serde_json::Value::as_bool) != Some(true) {
        return Err(EngineError::Handshake {
            frame: frame.to_string(),
        });
    }
    let pid = frame
        .get("pid")
        .and_then(serde_json::Value::as_u64)
        .and_then(|p| u32::try_from(p).ok());
    Ok(crate::exec::transport::ReadyInfo { pid })
}

/// Write a length-prefixed (u32 LE) JSON frame.
///
/// The bincode-vs-msgpack decision (ADR-E002) is deferred; JSON framing is adequate at this scale
/// and was validated in the Phase-1 spike.
pub fn write_frame<W: Write, T: Serialize>(w: &mut W, msg: &T) -> Result<()> {
    let bytes = serde_json::to_vec(msg)?;
    let len =
        u32::try_from(bytes.len()).map_err(|_| EngineError::Protocol("frame too large".into()))?;
    w.write_all(&len.to_le_bytes())?;
    w.write_all(&bytes)?;
    w.flush()?;
    Ok(())
}

/// Read a length-prefixed (u32 LE) JSON frame. `Ok(None)` on a clean EOF (peer closed).
pub fn read_frame<R: Read, T: DeserializeOwned>(r: &mut R) -> Result<Option<T>> {
    let mut header = [0u8; 4];
    if let Err(e) = r.read_exact(&mut header) {
        if e.kind() == io::ErrorKind::UnexpectedEof {
            return Ok(None);
        }
        return Err(EngineError::Io(e));
    }
    let len = u32::from_le_bytes(header) as usize;
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    Ok(Some(serde_json::from_slice(&buf)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_roundtrips_request_to_response_shape() {
        let node = NodeId::new("m.py::t");
        let req = ExecRequest::bare(&node, TestStyle::Function, 5000);
        let mut buf = Vec::new();
        write_frame(&mut buf, &req).unwrap();
        // Header is the LE length of the JSON payload.
        let declared = u32::from_le_bytes(buf[..4].try_into().unwrap()) as usize;
        assert_eq!(declared, buf.len() - 4);

        // A response-shaped value reads back through the same framing.
        let mut out = Vec::new();
        let resp = serde_json::json!({"node_id": "m.py::t", "outcome": "passed", "detail": ""});
        write_frame(&mut out, &resp).unwrap();
        let mut cursor = io::Cursor::new(out);
        let back: ExecResponse = read_frame(&mut cursor).unwrap().unwrap();
        assert_eq!(back.node_id.as_str(), "m.py::t");
        assert_eq!(back.outcome, Outcome::Passed);
        assert_eq!(back.skip_origin, None);
    }

    #[test]
    fn read_frame_on_empty_is_none() {
        let mut empty = io::Cursor::new(Vec::<u8>::new());
        let got: Option<ExecResponse> = read_frame(&mut empty).unwrap();
        assert!(got.is_none());
    }
}
