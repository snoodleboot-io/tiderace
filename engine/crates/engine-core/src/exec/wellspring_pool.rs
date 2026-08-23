//! One imported Python image, N forked workers (TID-4).
//!
//! The old pool ran N independent `python <shim>` processes, so an 8-worker run imported the project
//! **eight times**. Wall clock hid it, because the imports overlap across workers; CPU did not. On a
//! large-import corpus that is ~2.6s per worker — roughly 21s of the ~34s of CPU that eight workers
//! add over one. On a laptop idle cores absorb that. On a CI runner billed for CPU, and often with
//! fewer cores than the default assumes, it is the whole cost.
//!
//! This pool imports once and forks the workers from that image, so every worker gets the imported
//! interpreter for the price of a page-table copy. It is the same primitive the engine already runs
//! on, one level up: the wellspring forks a pristine child per *test*; this forks a pristine worker
//! per *core* first.
//!
//! **Workers connect back** to a listening Unix socket rather than being handed inherited file
//! descriptors. Passing fds through `exec` needs `dup2` and clearing `CLOEXEC`, which means a `libc`
//! dependency in a crate that has none; having the children dial out needs neither, on either side.
//!
//! Semantics are unchanged from N separate wellsprings: each worker builds its own `Engine` *after*
//! the fork, so fixture state is per-worker exactly as before. What is shared is only what was
//! already immutable by then — the imported modules and the discovered fixture registry.

use std::io::BufReader;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use std::collections::HashSet;

use crate::domain::{TestItem, TestResult};
use crate::error::{EngineError, Result};
use crate::exec::transport::{run_batch, PipeTransport, ShimTransport};
use crate::exec::Worker;

/// A transport to one pooled worker.
pub type PooledTransport = PipeTransport<UnixStream, BufReader<UnixStream>>;

/// The parent process plus its accepted worker connections.
pub struct WellspringPool {
    parent: Child,
    socket_path: PathBuf,
    /// Handed out one at a time by [`take_worker`](Self::take_worker).
    workers: Vec<PooledTransport>,
}

impl WellspringPool {
    /// Launch the shim in pool mode and accept `size` worker connections.
    ///
    /// Blocks until every worker has connected and completed its readiness handshake, which is also
    /// what makes the import cost a one-off: the parent does not fork until the import is done, so
    /// the first accept implies the project is loaded.
    pub fn launch(
        python: &str,
        shim: &Path,
        root: &Path,
        restore: bool,
        size: usize,
    ) -> Result<Self> {
        let size = size.max(1);
        let socket_path = Self::socket_path();
        // A stale socket from a killed run would make `bind` fail; the path is unique per process
        // and sequence, so anything here is debris rather than a live listener.
        let _ = std::fs::remove_file(&socket_path);
        let listener = UnixListener::bind(&socket_path)
            .map_err(|e| EngineError::Exec(format!("failed to bind worker socket: {e}")))?;

        let mut cmd = Command::new(python);
        cmd.arg(shim)
            .arg(root)
            .arg("--pool")
            .arg(size.to_string())
            .arg("--connect")
            .arg(&socket_path);
        if restore {
            cmd.arg("--restore");
        }
        let parent = cmd
            // Pin native thread pools — threaded BLAS/OMP + fork() is a known hazard, and this
            // process forks twice over (workers, then a child per test).
            .env("OPENBLAS_NUM_THREADS", "1")
            .env("OMP_NUM_THREADS", "1")
            .env("MKL_NUM_THREADS", "1")
            // The protocol runs over the sockets, so stdout is free to carry diagnostics through to
            // the terminal the way stderr already does.
            .stdin(Stdio::null())
            .spawn()
            .map_err(|e| {
                let _ = std::fs::remove_file(&socket_path);
                EngineError::Exec(format!("failed to launch wellspring pool: {e}"))
            })?;

        let mut pool = Self {
            parent,
            socket_path,
            workers: Vec::with_capacity(size),
        };
        for i in 0..size {
            let (stream, _) = listener
                .accept()
                .map_err(|e| EngineError::Exec(format!("worker {i} never connected: {e}")))?;
            let read_half = stream
                .try_clone()
                .map_err(|e| EngineError::Exec(format!("worker {i} socket clone: {e}")))?;
            let mut transport = PipeTransport::new(stream, BufReader::new(read_half));
            transport.ready()?;
            pool.workers.push(transport);
        }
        Ok(pool)
    }

    /// Hand one worker's transport to a caller (typically one scheduler batch, on its own thread).
    pub fn take_worker(&mut self) -> Option<PooledTransport> {
        self.workers.pop()
    }

    /// How many workers are still unclaimed.
    pub fn available(&self) -> usize {
        self.workers.len()
    }

    /// The imported parent's pid, for diagnostics.
    pub fn pid(&self) -> u32 {
        self.parent.id()
    }

    fn socket_path() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "tiderace-pool-{}-{}.sock",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ))
    }
}

impl Drop for WellspringPool {
    fn drop(&mut self) {
        // Close every worker connection first: each one is a live `serve` loop that exits on EOF,
        // and the parent is blocked in `waitpid` on all of them. Reaping before they can see EOF
        // would deadlock — the same shutdown ordering `Wellspring` already depends on.
        self.workers.clear();
        let _ = self.parent.wait();
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

/// One pooled worker, driven exactly like a [`ForkWorker`](crate::exec::ForkWorker).
///
/// Holds a connection rather than a process: the process is a fork of the pool's imported parent,
/// which is the entire point. Every execution knob behaves identically, because the loop underneath
/// is the same `run_batch`.
pub struct PooledWorker {
    transport: PooledTransport,
    deadline_ms: u64,
    optimistic_no_fork: bool,
    trusted: HashSet<String>,
    must_fork: HashSet<String>,
}

impl PooledWorker {
    pub fn new(transport: PooledTransport, deadline_ms: u64) -> Self {
        Self {
            transport,
            deadline_ms,
            optimistic_no_fork: false,
            trusted: HashSet::new(),
            must_fork: HashSet::new(),
        }
    }

    /// Take the in-process ladder for restorable tests. Sound only because the pool always launches
    /// with `restore` — see [`WellspringPool::launch`], which mirrors `ForkWorker::launch_optimistic`
    /// in making the unsound combination unreachable.
    pub fn with_optimistic_no_fork(mut self, on: bool) -> Self {
        self.optimistic_no_fork = on;
        self
    }

    /// Node ids recorded pure and unchanged: bare no-fork, skipping the snapshot (TID-1).
    pub fn with_trusted_pure(mut self, trusted: HashSet<String>) -> Self {
        self.trusted = trusted;
        self
    }

    /// Node ids recorded as disturbing interpreter state: forked regardless of the ladder (TID-33).
    pub fn with_must_fork(mut self, must_fork: HashSet<String>) -> Self {
        self.must_fork = must_fork;
        self
    }
}

impl Worker for PooledWorker {
    fn run(&mut self, items: &[TestItem]) -> Result<Vec<TestResult>> {
        run_batch(
            &mut self.transport,
            items,
            self.deadline_ms,
            self.optimistic_no_fork,
            &self.trusted,
            &self.must_fork,
        )
    }
}
