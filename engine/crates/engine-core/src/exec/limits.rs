//! Every margin, deadline, floor and fallback the execution layer runs on, in one place, each
//! with where its number came from (TID-115). They were in six files, and the per-test
//! deadline had two defaults.

use std::time::Duration;

/// The default per-test deadline.
///
/// A deadline exists to catch a **hang**, not to enforce speed, and 5s was doing the latter: pytest
/// ships no per-test timeout at all (`pytest-timeout` is opt-in), and suites that do set one
/// conventionally pick 60s — `pirn-agents` runs its own CI with `pytest --timeout=60`. At 5s a test
/// that legitimately shells out to a fresh interpreter failed here and passed under pytest, which is
/// a wrong red arriving through configuration rather than logic (TID-28). `--timeout` overrides it.
pub const DEFAULT_DEADLINE_MS: u64 = 60_000;

/// Compile-time floor on the above. Tuning stays possible, but lowering it back into the seconds
/// range has to confront the reasoning: a subprocess-spawning test on a large-import corpus needs an
/// interpreter start plus the project import (~2.6s measured) before it does any work.
const _: () = assert!(
    DEFAULT_DEADLINE_MS >= 30_000,
    "the default deadline must not be tight enough to fail slow-but-valid tests (TID-28)"
);

/// How long past the per-test deadline a worker may stay silent before it is given up on
/// (TID-93): the deadline itself is the shim's to enforce; this is the engine's margin over it.
pub const LOST_WORKER_MARGIN_MS: u64 = 10_000;

/// [`LOST_WORKER_MARGIN_MS`] as a duration.
pub const LOST_WORKER_MARGIN: Duration = Duration::from_millis(LOST_WORKER_MARGIN_MS);

/// How long the pool's parent may spend importing the project before the first worker connects.
/// Generous on purpose: a false timeout here would break a legitimate large project, while a dead
/// parent is caught immediately by the liveness check and never has to wait this out.
pub const IMPORT_DEADLINE: Duration = Duration::from_secs(300);

/// How long the remaining pooled workers may take once the first has connected. They are forks of
/// an image that has already imported everything, so they arrive within milliseconds; this is
/// slack, not a budget.
pub const WORKER_DEADLINE: Duration = Duration::from_secs(30);

/// How often the pool's accept loop checks whether its parent is still alive.
pub const POOL_POLL: Duration = Duration::from_millis(5);

/// The machine's parallelism, falling back to 4 where it cannot be read: the default worker
/// count and the sub-interpreter pool's default size.
pub fn default_parallelism() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
}
