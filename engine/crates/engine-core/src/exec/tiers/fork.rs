use std::collections::HashSet;
use std::path::Path;

use crate::domain::{NodeId, TestItem, TestResult};
use crate::error::Result;
use crate::exec::process::{ShimLaunch, ShimMode, ShimProcess, ShimTarget};
use crate::exec::transport::{run_batch, Live, PipeTransport, ShimTransport};
use crate::exec::worker::Worker;

/// Default executor (Linux/macOS): one warm wellspring — a Python process that imports the
/// project once — and a forked, pristine copy-on-write child per test (ADR-E003).
///
/// Fields drop in order: the transport first, which closes the shim's stdin (EOF → it runs its
/// wider-scope finalizers and exits), then the process, which reaps it. The other order would
/// deadlock: a shim blocked in `read` and an engine blocked in `wait`.
pub struct ForkWorker {
    transport: Live,
    process: ShimProcess,
    deadline_ms: u64,
    optimistic_no_fork: bool,
    trusted: HashSet<NodeId>,
    /// Node ids recorded as disturbing interpreter state — forked even under the ladder (TID-33).
    must_fork: HashSet<NodeId>,
}

impl ForkWorker {
    /// Launch the worker against `root` (the directory placed on the shim's `sys.path`).
    ///
    /// Forks every test. To take the in-process ladder where it is sound, use
    /// [`launch_optimistic`](ForkWorker::launch_optimistic) — which is the only way to enable it,
    /// because enabling it on a wellspring launched without restore is not sound.
    pub fn launch(python: &str, shim: &Path, root: &Path) -> Result<Self> {
        Self::launch_selected(python, shim, root, false, None)
    }

    /// Launch with snapshot/restore on AND the optimistic in-process ladder enabled.
    ///
    /// The two go together or not at all. `with_optimistic_no_fork(true)` on a plain
    /// [`launch`](ForkWorker::launch) leaves the shim with no snapshot and no COW copy, so a test
    /// taking the no-fork path leaks its mutations into every later test on that module — and the
    /// shim's `must_fork` restorability check is itself gated on restore, so an unrestorable module
    /// would run in-process too. Pairing them here makes that combination unconstructible.
    pub fn launch_optimistic(python: &str, shim: &Path, root: &Path) -> Result<Self> {
        Self::launch_selected(python, shim, root, true, None)
    }

    /// Launch with the ladder on or off, importing only the modules named in `modules` (TID-75).
    pub fn launch_selected(
        python: &str,
        shim: &Path,
        root: &Path,
        optimistic: bool,
        modules: Option<&Path>,
    ) -> Result<Self> {
        Self::launch_target(&ShimTarget::new(python, shim, root), optimistic, modules)
    }

    /// [`launch_selected`](Self::launch_selected) against a [`ShimTarget`]. `optimistic` launches
    /// the shim with snapshot/restore *and* enables the in-process ladder — the two go together
    /// or not at all (see [`launch_optimistic`](Self::launch_optimistic)).
    pub fn launch_target(
        target: &ShimTarget,
        optimistic: bool,
        modules: Option<&Path>,
    ) -> Result<Self> {
        let mut process = ShimLaunch::new(
            target,
            ShimMode::Serve {
                restore: optimistic,
            },
        )
        .modules(modules)
        .spawn()?;
        let (stdin, stdout) = process.take_pipes()?;
        let mut transport = PipeTransport::new(stdin, stdout);
        transport.ready()?;
        Ok(Self {
            transport,
            process,
            deadline_ms: 5_000,
            optimistic_no_fork: optimistic,
            trusted: HashSet::new(),
            must_fork: HashSet::new(),
        })
    }

    /// Per-test deadline (ms) after which the forked child is killed and reported as `Error`.
    pub fn with_deadline_ms(mut self, ms: u64) -> Self {
        self.deadline_ms = ms;
        self
    }

    /// Ask the shim to run tests in-process where sound (the snapshot/restore fast path). The shim
    /// still forks any module it can't snapshot-restore, so isolation is preserved. The wellspring
    /// must have been launched with `TIDERACE_RESTORE=1` (the daemon sets it) for this to be safe.
    pub fn with_optimistic_no_fork(mut self, on: bool) -> Self {
        self.optimistic_no_fork = on;
        self
    }

    /// Node ids known to be *pure and unchanged* (TID-1): each runs BARE no-fork (skip the snapshot).
    /// Only honored together with `with_optimistic_no_fork(true)`.
    pub fn with_trusted_pure(mut self, trusted: HashSet<NodeId>) -> Self {
        self.trusted = trusted;
        self
    }

    /// Node ids recorded as disturbing interpreter state (TID-33): each is forked even under
    /// `with_optimistic_no_fork(true)`. The shim catches a first offence on its own and re-runs it
    /// forked; this is what stops paying for that discovery on every subsequent run.
    pub fn with_must_fork(mut self, must_fork: HashSet<NodeId>) -> Self {
        self.must_fork = must_fork;
        self
    }

    /// The wellspring's pid: the parent of every per-test fork.
    pub fn wellspring_pid(&self) -> Option<u32> {
        Some(self.process.pid())
    }
}

impl Worker for ForkWorker {
    fn run(&mut self, items: &[TestItem]) -> Result<Vec<TestResult>> {
        let deadline_ms = self.deadline_ms;
        let nf = self.optimistic_no_fork;
        run_batch(
            &mut self.transport,
            items,
            deadline_ms,
            nf,
            &self.trusted,
            &self.must_fork,
        )
    }
}
