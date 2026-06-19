use std::io::{self, Read, Write};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

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
    pub node_id: &'a str,
    /// Wire token for the test style (`pytest_func` / `pytest_method` / `unittest_method`).
    pub style: &'a str,
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
}

impl<'a> ExecRequest<'a> {
    /// A Phase-2-shaped (fixtureless) request: the three wire fields, empty fixture fields. Keeps
    /// existing call sites concise and the frame byte-identical to Phase 2.
    pub fn bare(node_id: &'a str, style: &'a str, deadline_ms: u64) -> Self {
        Self {
            node_id,
            style,
            deadline_ms,
            post_fork: Vec::new(),
            reinit: Vec::new(),
            fixture_args: FixtureArgs::new(),
        }
    }
}

/// The child's reported outcome for one test.
#[derive(Debug, Deserialize)]
pub struct ExecResponse {
    pub node_id: String,
    /// Wire outcome token; parse with [`crate::domain::Outcome::from_wire`].
    pub outcome: String,
    #[serde(default)]
    pub detail: String,
}

/// Write a length-prefixed (u32 LE) JSON frame.
///
/// The bincode-vs-msgpack decision (ADR-E002) is deferred; JSON framing is adequate at this scale
/// and was validated in the Phase-1 spike.
pub fn write_frame<W: Write, T: Serialize>(w: &mut W, msg: &T) -> Result<()> {
    let bytes = serde_json::to_vec(msg).map_err(|e| EngineError::Exec(e.to_string()))?;
    let len =
        u32::try_from(bytes.len()).map_err(|_| EngineError::Exec("frame too large".into()))?;
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
    let msg = serde_json::from_slice(&buf).map_err(|e| EngineError::Exec(e.to_string()))?;
    Ok(Some(msg))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_roundtrips_request_to_response_shape() {
        let req = ExecRequest::bare("m.py::t", "pytest_func", 5000);
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
        assert_eq!(back.node_id, "m.py::t");
        assert_eq!(back.outcome, "passed");
    }

    #[test]
    fn read_frame_on_empty_is_none() {
        let mut empty = io::Cursor::new(Vec::<u8>::new());
        let got: Option<ExecResponse> = read_frame(&mut empty).unwrap();
        assert!(got.is_none());
    }
}
