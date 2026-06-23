use std::io::{self, Read, Write};

use crate::rpc_method::{RpcRequest, RpcResponse};

/// Handles one decoded RPC request, producing a response. The daemon implements this over its warm
/// state (session/wellspring/cache); tests implement it as a pure double. Keeping it a trait makes the
/// connection loop testable with no socket and no live engine.
pub trait RpcHandler {
    fn handle(&mut self, request: RpcRequest) -> RpcResponse;
}

/// Serve one client connection: read length-prefixed JSON request frames, dispatch each to `handler`,
/// write the response frame, until EOF or a `Shutdown` request (design 08 `rpc_server.rs`). Generic
/// over any `Read + Write`, so a real socket and an in-memory test stream drive the *same* loop.
/// Returns `Ok(true)` if the client asked the daemon to shut down, `Ok(false)` on a clean disconnect.
pub fn serve_connection<S: Read + Write>(
    mut stream: S,
    handler: &mut dyn RpcHandler,
) -> io::Result<bool> {
    loop {
        let request: RpcRequest = match read_frame(&mut stream)? {
            Some(req) => req,
            None => return Ok(false), // client disconnected cleanly
        };
        let shutdown = matches!(request, RpcRequest::Shutdown);
        let response = handler.handle(request);
        write_frame(&mut stream, &response)?;
        if shutdown {
            return Ok(true);
        }
    }
}

/// Write a length-prefixed (u32 LE) JSON frame — the same framing the shim transport uses, kept
/// local so the daemon's wire protocol is self-contained.
pub fn write_frame<W: Write, T: serde::Serialize>(w: &mut W, msg: &T) -> io::Result<()> {
    let bytes = serde_json::to_vec(msg).map_err(io::Error::other)?;
    let len = u32::try_from(bytes.len()).map_err(|_| io::Error::other("frame too large"))?;
    w.write_all(&len.to_le_bytes())?;
    w.write_all(&bytes)?;
    w.flush()
}

/// Read a length-prefixed (u32 LE) JSON frame. `Ok(None)` on a clean EOF before any bytes.
pub fn read_frame<R: Read, T: serde::de::DeserializeOwned>(r: &mut R) -> io::Result<Option<T>> {
    let mut header = [0u8; 4];
    if let Err(e) = r.read_exact(&mut header) {
        if e.kind() == io::ErrorKind::UnexpectedEof {
            return Ok(None);
        }
        return Err(e);
    }
    let len = u32::from_le_bytes(header) as usize;
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    serde_json::from_slice(&buf)
        .map(Some)
        .map_err(io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpc_method::RpcResult;

    /// An in-memory bidirectional stream: serves preloaded request frames, captures written responses.
    struct Duplex {
        inbox: io::Cursor<Vec<u8>>,
        outbox: Vec<u8>,
    }

    impl Read for Duplex {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.inbox.read(buf)
        }
    }
    impl Write for Duplex {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.outbox.write(buf)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// A double that answers Discover and Run, and records what it was asked.
    struct FakeHandler {
        seen: Vec<String>,
    }
    impl RpcHandler for FakeHandler {
        fn handle(&mut self, request: RpcRequest) -> RpcResponse {
            match request {
                RpcRequest::Discover => {
                    self.seen.push("discover".into());
                    RpcResponse::Discovered {
                        node_ids: vec!["t.py::a".into()],
                    }
                }
                RpcRequest::Run { node_ids } => {
                    self.seen.push(format!("run:{}", node_ids.len()));
                    RpcResponse::Ran {
                        results: node_ids
                            .into_iter()
                            .map(|n| RpcResult {
                                node_id: n,
                                outcome: "passed".into(),
                                duration_ms: 1,
                            })
                            .collect(),
                    }
                }
                RpcRequest::Shutdown => {
                    self.seen.push("shutdown".into());
                    RpcResponse::ShuttingDown
                }
                _ => RpcResponse::Error {
                    message: "unsupported".into(),
                },
            }
        }
    }

    fn framed(reqs: &[RpcRequest]) -> Vec<u8> {
        let mut buf = Vec::new();
        for r in reqs {
            write_frame(&mut buf, r).unwrap();
        }
        buf
    }

    fn responses(bytes: &[u8]) -> Vec<RpcResponse> {
        let mut cur = io::Cursor::new(bytes.to_vec());
        let mut out = Vec::new();
        while let Some(resp) = read_frame::<_, RpcResponse>(&mut cur).unwrap() {
            out.push(resp);
        }
        out
    }

    #[test]
    fn dispatches_requests_until_shutdown() {
        let inbox = framed(&[
            RpcRequest::Discover,
            RpcRequest::Run {
                node_ids: vec!["t.py::a".into(), "t.py::b".into()],
            },
            RpcRequest::Shutdown,
        ]);
        let mut stream = Duplex {
            inbox: io::Cursor::new(inbox),
            outbox: Vec::new(),
        };
        let mut handler = FakeHandler { seen: Vec::new() };

        let shutdown = serve_connection(&mut stream, &mut handler).unwrap();

        assert!(shutdown, "Shutdown request returns true");
        assert_eq!(handler.seen, vec!["discover", "run:2", "shutdown"]);
        let resps = responses(&stream.outbox);
        assert!(matches!(resps[0], RpcResponse::Discovered { .. }));
        assert!(matches!(&resps[1], RpcResponse::Ran { results } if results.len() == 2));
        assert!(matches!(resps[2], RpcResponse::ShuttingDown));
    }

    #[test]
    fn clean_disconnect_returns_false() {
        let mut stream = Duplex {
            inbox: io::Cursor::new(framed(&[RpcRequest::Health])),
            outbox: Vec::new(),
        };
        let mut handler = FakeHandler { seen: Vec::new() };
        // Health isn't handled by the fake → Error response, then EOF (no shutdown).
        let shutdown = serve_connection(&mut stream, &mut handler).unwrap();
        assert!(!shutdown);
        assert!(matches!(
            responses(&stream.outbox)[0],
            RpcResponse::Error { .. }
        ));
    }
}
