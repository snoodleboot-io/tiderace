use std::os::unix::net::UnixListener;
use std::path::Path;

use crate::rpc_server::{serve_connection, RpcHandler};

/// Bind a per-project Unix socket and serve clients until one requests shutdown (design 08 `socket.rs`:
/// per-user, per-project, local-socket only). Thin OS glue over [`serve_connection`] — the framing +
/// dispatch logic it drives is unit-tested in `rpc_server`; this just owns the listener lifecycle.
pub fn serve_unix_socket(path: &Path, handler: &mut dyn RpcHandler) -> std::io::Result<()> {
    let _ = std::fs::remove_file(path); // clear a stale socket left by a crashed daemon
    let listener = UnixListener::bind(path)?;
    for conn in listener.incoming() {
        if serve_connection(conn?, handler)? {
            break; // a client asked the daemon to shut down
        }
    }
    let _ = std::fs::remove_file(path);
    Ok(())
}
