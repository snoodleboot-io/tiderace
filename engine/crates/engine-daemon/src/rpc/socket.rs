use std::path::Path;

use crate::rpc::server::{serve_connection, RpcHandler};

/// Where a daemon serving `root` listens, and where `tiderace run` looks for one (TID-84):
/// `<tmp>/tiderace-<uid>/<digest of the canonical root>.sock`. Not under the root itself — a Unix
/// socket path is limited to ~108 bytes, and a project's path can be longer than that.
pub fn daemon_socket_path(root: &Path) -> std::path::PathBuf {
    use std::hash::{Hash, Hasher};
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let mut h = std::collections::hash_map::DefaultHasher::new();
    root.hash(&mut h);
    #[cfg(unix)]
    let uid = unsafe { libc_getuid() };
    #[cfg(not(unix))]
    let uid = 0u32;
    std::env::temp_dir()
        .join(format!("tiderace-{uid}"))
        .join(format!("{:016x}.sock", h.finish()))
}

#[cfg(unix)]
unsafe fn libc_getuid() -> u32 {
    unsafe extern "C" {
        fn getuid() -> u32;
    }
    unsafe { getuid() }
}

/// Bind a per-project Unix socket and serve clients until one requests shutdown (design 08 `socket.rs`:
/// per-user, per-project, local-socket only). Thin OS glue over [`serve_connection`] — the framing +
/// dispatch logic it drives is unit-tested in `rpc_server`; this just owns the listener lifecycle.
#[cfg(unix)]
pub fn serve_unix_socket(path: &Path, handler: &mut dyn RpcHandler) -> std::io::Result<()> {
    use std::os::unix::net::UnixListener;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?; // `<root>/.tiderace-cache/` may not exist yet (TID-84)
    }
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
