//! TID-35 — the sub-interpreter safety cache works from the CLI, not only from the daemon.
//!
//! Classifying a module means launching a fresh interpreter and trying to import it there. That is
//! the only way to know whether it can load in a sub-interpreter at all, and it is expensive. The
//! verdicts were cached, but the cache lived in the *daemon's* persisted state, so
//! `tiderace run --strategy subinterp` paid a full probe pass on every single invocation.
//!
//! On the 20-module corpus this tier was measured against, that probe pass is most of its fixed
//! cost — which means the tier's published numbers were partly measuring the probe rather than the
//! tier. And it lands hardest in the one place the tier exists for: Windows, where there is no fork,
//! no daemon assumption to lean on, and the alternative is running sequentially.
//!
//! These tests drive the cache through its real interface rather than asserting on a timing, which
//! on a shared runner would prove nothing.

use engine_core::exec::SafeSetCache;
use engine_core::testing::skip_live;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repo root")
}

fn shim() -> PathBuf {
    repo_root().join("engine/py-shim/shim.py")
}

/// The probe needs CPython 3.14's interpreter API; anything older answers "undeterminable" for every
/// module and the tier falls back, so there would be nothing to cache.
fn fx_venv() -> Option<String> {
    let p = repo_root().join(".tiderace-fx-venv/bin/python");
    p.exists().then(|| p.to_string_lossy().into_owned())
}

fn temp(tag: &str) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "tiderace_t35_{tag}_{}_{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A second run reuses the first run's verdicts, and an edit re-probes just the edited module.
///
/// This is the whole ticket: the classification must survive *process exit*, because a CLI run is a
/// process that exits. Asserting through a save/load round trip is what distinguishes this from an
/// in-memory cache that helps nobody.
#[test]
fn a_second_cli_run_reuses_the_first_runs_verdicts() {
    let Some(python) = fx_venv() else {
        skip_live("`.tiderace-fx-venv` (CPython 3.14 + numpy) not present");
        return;
    };
    let dir = temp("reuse");
    std::fs::write(dir.join("test_pure.py"), "def test_a():\n    assert True\n").unwrap();
    std::fs::write(
        dir.join("test_np.py"),
        "import numpy\ndef test_n():\n    assert numpy.array([1]).sum() == 1\n",
    )
    .unwrap();
    let modules = vec!["test_pure.py".to_string(), "test_np.py".to_string()];

    // First run: cold. Both modules must be probed, and the classification must be right — a pure
    // module is sub-interp-safe, one importing numpy (a single-phase C extension) is not.
    let mut first = SafeSetCache::load(&dir);
    assert_eq!(
        first.pending_probes(&dir, &modules),
        2,
        "a cold cache must probe everything"
    );
    let safe = first
        .resolve(&python, &shim(), &dir, &modules)
        .expect("probe runs");
    assert!(safe.contains("test_pure.py"), "pure module is safe");
    assert!(!safe.contains("test_np.py"), "numpy module is not safe");
    first.save(&dir).expect("cache persists");

    // Second run: a different process would load from disk. Nothing left to probe.
    let mut second = SafeSetCache::load(&dir);
    assert_eq!(
        second.pending_probes(&dir, &modules),
        0,
        "TID-35: the second run must not re-probe anything"
    );
    let safe2 = second
        .resolve(&python, &shim(), &dir, &modules)
        .expect("resolve from cache");
    assert_eq!(safe, safe2, "a cached answer must equal the probed one");

    // Editing one module re-probes exactly that one. Verdicts are keyed by content, so a changed
    // module is a different question — and an unchanged one must not pay for its neighbour.
    std::fs::write(
        dir.join("test_pure.py"),
        "def test_a():\n    assert True  # edited\n",
    )
    .unwrap();
    let third = SafeSetCache::load(&dir);
    assert_eq!(
        third.pending_probes(&dir, &modules),
        1,
        "only the edited module re-probes"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// An unwritable tree must still run — just without the speedup next time.
///
/// The cache is an optimisation, and an optimisation that can fail a run is a bug. A read-only
/// checkout is an ordinary thing (a CI cache mount, a container layer), and it must degrade to
/// "probe every time" rather than to an error.
#[test]
fn an_unwritable_tree_degrades_instead_of_failing() {
    let dir = temp("readonly");
    let cache = SafeSetCache::default();
    // A path that cannot be written to, because its parent does not exist.
    let missing = dir.join("no-such-directory");
    assert!(
        cache.save(&missing).is_err(),
        "the save genuinely fails here, or this test proves nothing"
    );
    // …and loading from it is a cold start, not a panic or an error.
    assert_eq!(
        SafeSetCache::load(&missing).pending_probes(&missing, &[]),
        0
    );
    let _ = std::fs::remove_dir_all(&dir);
}
