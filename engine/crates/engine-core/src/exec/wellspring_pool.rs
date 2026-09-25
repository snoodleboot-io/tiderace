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
use std::time::{Duration, Instant};

use std::collections::HashSet;

use crate::domain::{TestItem, TestResult};
use crate::error::{EngineError, Result};
use crate::exec::transport::{run_batch, PipeTransport, ShimTransport};
use crate::exec::Worker;

/// How long the parent may spend importing the project before the first worker connects. Generous on
/// purpose: a false timeout here would break a legitimate large project, while a dead parent is caught
/// immediately by the liveness check and never has to wait this out.
const IMPORT_DEADLINE: Duration = Duration::from_secs(300);

/// How long the remaining workers may take once the first has connected. They are forks of an image
/// that has already imported everything, so they arrive within milliseconds; this is slack, not a
/// budget.
const WORKER_DEADLINE: Duration = Duration::from_secs(30);

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
        Self::launch_selected(python, shim, root, restore, size, None)
    }

    /// As [`launch`](Self::launch), with the parent importing only the modules named in `modules`
    /// (a file of suite-relative paths, one per line) and the conftests above them (TID-75).
    pub fn launch_selected(
        python: &str,
        shim: &Path,
        root: &Path,
        restore: bool,
        size: usize,
        modules: Option<&Path>,
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
        if let Some(file) = modules {
            cmd.arg("--modules").arg(file);
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
        // Non-blocking, so the wait below can notice the parent dying instead of sitting in `accept`
        // forever. That is exactly what happened before (TID-43): the shim crashed during discovery on
        // flask, and the default configuration hung indefinitely with its python child a zombie.
        listener
            .set_nonblocking(true)
            .map_err(|e| EngineError::Exec(format!("worker socket: {e}")))?;
        let started = Instant::now();
        let mut first_connected: Option<Instant> = None;
        for i in 0..size {
            let stream = pool.accept_worker(&listener, i, size, started, &mut first_connected)?;
            let read_half = stream
                .try_clone()
                .map_err(|e| EngineError::Exec(format!("worker {i} socket clone: {e}")))?;
            let mut transport = PipeTransport::new(stream, BufReader::new(read_half));
            transport.ready()?;
            pool.workers.push(transport);
        }
        Ok(pool)
    }

    /// Wait for worker `i` to connect, failing fast if it never will.
    ///
    /// Two clocks, because startup has two phases with very different expected durations:
    ///
    /// * **Before the first worker connects**, the parent is importing the project. That is the slow
    ///   part and legitimately so — 2.6s on a large-import corpus, and it can be far longer — so it
    ///   gets a generous deadline. It also gets an immediate check on the parent: if the parent has
    ///   exited, no worker can ever arrive, and waiting out the deadline would just be a slower hang.
    /// * **Once one worker has connected**, the import is finished and every other worker is a fork of
    ///   that same image, arriving within milliseconds. A missing one after that means a worker died
    ///   on its way in. The parent is still alive in that case — it is busy waiting on the others —
    ///   so a liveness check alone would never fire, and only the short deadline catches it.
    fn accept_worker(
        &mut self,
        listener: &UnixListener,
        i: usize,
        size: usize,
        started: Instant,
        first_connected: &mut Option<Instant>,
    ) -> Result<UnixStream> {
        loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    // Accepted sockets are blocking on Linux regardless of the listener, but the
                    // transport's reads rely on it, so do not leave that to platform behaviour.
                    stream
                        .set_nonblocking(false)
                        .map_err(|e| EngineError::Exec(format!("worker {i} socket: {e}")))?;
                    first_connected.get_or_insert_with(Instant::now);
                    return Ok(stream);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) => {
                    return Err(EngineError::Exec(format!(
                        "worker {i} of {size} failed to connect: {e}"
                    )))
                }
            }

            if let Ok(Some(status)) = self.parent.try_wait() {
                return Err(EngineError::Exec(format!(
                    "the wellspring pool exited ({status}) before worker {} of {size} connected. The \
                     Python traceback above says why. If this suite only fails under the shared-import \
                     pool, --no-shared-import runs it with one interpreter per worker instead",
                    i + 1
                )));
            }

            let (clock, limit, phase) = match first_connected {
                None => (started, IMPORT_DEADLINE, "importing the project"),
                Some(t) => (*t, WORKER_DEADLINE, "starting its workers"),
            };
            if clock.elapsed() > limit {
                return Err(EngineError::Exec(format!(
                    "the wellspring pool was still {phase} after {}s with only {i} of {size} workers \
                     connected, so it was stopped rather than waited on indefinitely",
                    limit.as_secs()
                )));
            }
            std::thread::sleep(Duration::from_millis(5));
        }
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
