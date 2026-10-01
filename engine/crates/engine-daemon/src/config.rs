//! What the daemon is configured with, read once at start-up (TID-119). The handler used to read
//! `TIDERACE_CACHE_DIR` in its constructor and `TIDERACE_FORCE_FORK` / `TIDERACE_SUBINTERP` on
//! every run — three times per run — so no test could pin a configuration without mutating the
//! process environment.

use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonConfig {
    pub python: String,
    pub shim: PathBuf,
    pub root: PathBuf,
    /// A directory for the content-addressed result cache (ADR-E004, TID-7): a CI cache path or
    /// a shared mount, which makes a result computed on one machine a free hit on any other with
    /// the same inputs. `None` ⇒ cache off (impact-skip only).
    pub cache_dir: Option<PathBuf>,
    /// Take the optimistic in-process ladder (the default); off, fork-per-test — a debug and
    /// benchmark escape (`TIDERACE_FORCE_FORK=1`), not a user-facing flag, so the fork baseline
    /// stays measurable for regression checks.
    pub optimistic_no_fork: bool,
    /// Route sub-interpreter-safe modules through the parallel sub-interpreter pool on a full
    /// run (ADR-E015 / TID-11, `TIDERACE_SUBINTERP=1`). Its purpose is Windows parallelism.
    pub subinterp: bool,
    /// Where `serve` listens, when not the per-project default (`TIDERACE_SOCKET`).
    pub socket: Option<PathBuf>,
}

impl DaemonConfig {
    /// Everything on by default except the cache and the sub-interpreter tier.
    pub fn new(
        python: impl Into<String>,
        shim: impl Into<PathBuf>,
        root: impl Into<PathBuf>,
    ) -> Self {
        Self {
            python: python.into(),
            shim: shim.into(),
            root: root.into(),
            cache_dir: None,
            optimistic_no_fork: true,
            subinterp: false,
            socket: None,
        }
    }

    /// Where a daemon with this configuration listens: the override, else the per-project path
    /// (see [`daemon_socket_path`](crate::daemon_socket_path)).
    pub fn socket_path(&self) -> PathBuf {
        self.socket
            .clone()
            .unwrap_or_else(|| crate::rpc::socket::daemon_socket_path(&self.root))
    }

    /// [`new`](Self::new), with `TIDERACE_CACHE_DIR`, `TIDERACE_FORCE_FORK`, `TIDERACE_SUBINTERP`
    /// and `TIDERACE_SOCKET` read from the environment — the one place the daemon reads them.
    pub fn from_env(
        python: impl Into<String>,
        shim: impl Into<PathBuf>,
        root: impl Into<PathBuf>,
    ) -> Self {
        Self {
            cache_dir: std::env::var("TIDERACE_CACHE_DIR")
                .ok()
                .filter(|s| !s.is_empty())
                .map(PathBuf::from),
            optimistic_no_fork: std::env::var("TIDERACE_FORCE_FORK").as_deref() != Ok("1"),
            subinterp: std::env::var("TIDERACE_SUBINTERP").as_deref() == Ok("1"),
            socket: std::env::var("TIDERACE_SOCKET")
                .ok()
                .filter(|s| !s.is_empty())
                .map(PathBuf::from),
            ..Self::new(python, shim, root)
        }
    }
}
