use std::io::{self, Read, Write};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::error::{EngineError, Result};

/// A request to execute one test in a forked child of the Wellspring.
#[derive(Debug, Serialize)]
pub struct ExecRequest<'a> {
    pub node_id: &'a str,
    /// Wire token for the test style (`pytest_func` / `pytest_method` / `unittest_method`).
    pub style: &'a str,
    pub deadline_ms: u64,
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
        let req = ExecRequest {
            node_id: "m.py::t",
            style: "pytest_func",
            deadline_ms: 5000,
        };
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
