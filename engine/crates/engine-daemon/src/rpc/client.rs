//! The client side of the daemon's wire (TID-84, TID-120): one type that finds the socket,
//! frames a request and reads the answer, so a front end asks the daemon questions instead of
//! matching response variants at every call site.

use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::time::{Duration, Instant};

use engine_core::domain::TestResult;
use engine_core::exec::Selection;

use crate::error::DaemonError;
use crate::rpc::method::{RpcRequest, RpcResponse};
use crate::rpc::socket::daemon_socket_path;

/// A daemon's answer to `Health`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Healthy {
    pub pid: u32,
    /// Whether the warm image is held: a cold daemon imports the suite on its first run.
    pub warm: bool,
}

/// A daemon this client just started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Started {
    pub pid: u32,
    pub log: PathBuf,
}

/// The client for the daemon serving one root, whether or not one is listening.
#[derive(Debug, Clone)]
pub struct DaemonClient {
    root: PathBuf,
    socket: PathBuf,
}

/// How long `start` waits for the daemon to answer before giving up.
#[cfg(unix)]
const START_TIMEOUT: Duration = Duration::from_secs(30);

impl DaemonClient {
    /// The client for `root`, canonicalised when it exists — the socket path is a digest of the
    /// canonical root, so a relative path and its absolute spelling find the same daemon.
    pub fn for_root(root: &Path) -> Self {
        let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        let socket = daemon_socket_path(&root);
        Self { root, socket }
    }

    /// The root this client serves.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Where the daemon listens, or would.
    pub fn socket_path(&self) -> &Path {
        &self.socket
    }

    /// One request: `None` when there is no socket or nothing answers on it.
    #[cfg(unix)]
    pub fn call(&self, request: RpcRequest) -> Option<RpcResponse> {
        use std::os::unix::net::UnixStream;
        let mut stream = UnixStream::connect(&self.socket).ok()?;
        crate::rpc::server::write_frame(&mut stream, &request).ok()?;
        crate::rpc::server::read_frame::<_, RpcResponse>(&mut stream)
            .ok()
            .flatten()
    }

    /// The daemon serves over a Unix socket; elsewhere nothing is ever listening.
    #[cfg(not(unix))]
    pub fn call(&self, request: RpcRequest) -> Option<RpcResponse> {
        let _ = request;
        None
    }

    /// Whether a daemon is serving, and what it says about itself.
    pub fn health(&self) -> Option<Healthy> {
        match self.call(RpcRequest::Health)? {
            RpcResponse::Healthy { pid, warm } => Some(Healthy { pid, warm }),
            _ => None,
        }
    }

    /// The full run through the daemon: `None` when none is serving this root, `Some(Err)` when
    /// one is and could not run it, `Some(Ok(results))` otherwise — what the run in this process
    /// would have produced.
    pub fn run_full(&self, selection: &Selection) -> Option<Result<Vec<TestResult>, DaemonError>> {
        self.health()?;
        let request = RpcRequest::RunFull {
            keyword: selection.keyword.clone(),
            marker: selection.marker.clone(),
            strict_markers: selection.strict_markers,
        };
        Some(match self.call(request) {
            Some(RpcResponse::RanFull { results }) => Ok(results),
            Some(RpcResponse::Error { message }) => Err(DaemonError::Message(message)),
            Some(other) => Err(DaemonError::Message(format!(
                "unexpected answer: {other:?}"
            ))),
            None => Err(DaemonError::Message(
                "the daemon went away mid-run".to_string(),
            )),
        })
    }

    /// Ask the daemon to shut down: `true` when one was serving and agreed.
    pub fn shutdown(&self) -> bool {
        matches!(
            self.call(RpcRequest::Shutdown),
            Some(RpcResponse::ShuttingDown)
        )
    }

    /// Spawn `bin serve <root>` in its own process group with its output appended to `log`, and
    /// wait for it to answer. `Ok` carries the child's pid; a daemon already serving is not an
    /// error and is reported through [`health`](Self::health) first by callers that care.
    #[cfg(unix)]
    pub fn start(&self, bin: &Path, log: &Path) -> Result<Started, DaemonError> {
        use std::os::unix::process::CommandExt;
        if let Some(dir) = log.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log)
            .map_err(|e| DaemonError::Message(format!("cannot open {}: {e}", log.display())))?;
        let (out, err) = (file.try_clone()?, file);
        let child = std::process::Command::new(bin)
            .arg("serve")
            .arg(&self.root)
            .stdin(std::process::Stdio::null())
            .stdout(out)
            .stderr(err)
            .process_group(0) // its own group: a Ctrl-C in the caller's terminal does not take it down
            .spawn()
            .map_err(|e| DaemonError::Message(format!("cannot start {}: {e}", bin.display())))?;
        let deadline = Instant::now() + START_TIMEOUT;
        while Instant::now() < deadline {
            if self.health().is_some() {
                return Ok(Started {
                    pid: child.id(),
                    log: log.to_path_buf(),
                });
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Err(DaemonError::Message(format!(
            "the daemon did not answer on {} within {}s — see {}",
            self.socket.display(),
            START_TIMEOUT.as_secs(),
            log.display()
        )))
    }

    /// The daemon serves over a Unix socket, which this platform does not have.
    #[cfg(not(unix))]
    pub fn start(&self, bin: &Path, log: &Path) -> Result<Started, DaemonError> {
        let _ = (bin, log);
        Err(DaemonError::Message(
            "the daemon serves over a Unix socket, which this platform does not have".to_string(),
        ))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::rpc::server::RpcHandler;
    use crate::rpc::socket::serve_unix_socket;

    /// A server that answers `Health` and `Shutdown` and nothing else: the client's contract
    /// is the wire, not the engine.
    struct Stub;

    impl RpcHandler for Stub {
        fn handle(&mut self, request: RpcRequest) -> RpcResponse {
            match request {
                RpcRequest::Health => RpcResponse::Healthy {
                    pid: 4242,
                    warm: true,
                },
                RpcRequest::Shutdown => RpcResponse::ShuttingDown,
                other => RpcResponse::Error {
                    message: format!("stub: {other:?}"),
                },
            }
        }
    }

    #[test]
    fn nothing_listening_is_none_not_an_error() {
        let dir = std::env::temp_dir().join(format!("tiderace_client_none_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let client = DaemonClient::for_root(&dir);
        assert!(client.health().is_none());
        assert!(client.run_full(&Selection::default()).is_none());
        assert!(!client.shutdown());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn health_run_and_shutdown_go_over_the_socket() {
        let dir = std::env::temp_dir().join(format!("tiderace_client_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let client = DaemonClient::for_root(&dir);
        let socket = client.socket_path().to_path_buf();
        let server = std::thread::spawn(move || serve_unix_socket(&socket, &mut Stub));
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut healthy = None;
        while Instant::now() < deadline {
            healthy = client.health();
            if healthy.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(
            healthy,
            Some(Healthy {
                pid: 4242,
                warm: true
            })
        );
        // A run the stub refuses is `Some(Err)`: the daemon is there, it just could not run it.
        match client.run_full(&Selection::default()) {
            Some(Err(DaemonError::Message(m))) => assert!(m.contains("stub"), "{m}"),
            other => panic!("expected the stub's refusal, got {other:?}"),
        }
        assert!(client.shutdown(), "the stub agrees to shut down");
        server.join().unwrap().unwrap();
        assert!(client.health().is_none(), "nothing listens after shutdown");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
