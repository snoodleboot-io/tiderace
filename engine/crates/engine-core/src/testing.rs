//! Test-support for **live scenarios** — the ones that need a real Python (`.riptide-fx-venv`, a
//! CPython 3.14 with `concurrent.interpreters`, …) rather than a scripted stub.
//!
//! ## Why this exists
//!
//! Live scenarios self-skip when their interpreter is missing, because not every environment can run
//! them (the `engine · windows` job has no fx venv; a fresh clone has none either). But Rust's test
//! harness has no "skipped" state: an early `return` is reported as **`ok`**. A missing interpreter
//! therefore looks *identical to a pass* — and these are the scenarios that assert the engine's load-
//! bearing invariants (no-fork ≡ fork, sub-interp ≡ fork, purity detection). A green suite could mean
//! "the isolation ladder is sound" or "none of that ran"; nothing in the output distinguished them.
//!
//! That is not hypothetical. `.riptide-fx-venv/bin/python` was a symlink into a *versioned* VSCode
//! snap path (`snap/code/244/…`); when that revision was garbage-collected the venv broke, and
//! `cargo test --workspace` kept reporting `ok` with **10 live tests silently skipped**.
//!
//! ## The contract
//!
//! Call [`skip_live`] instead of `eprintln!("SKIP: …")`.
//!
//! * **Strict where it counts** — with `RIPTIDE_REQUIRE_LIVE=1` a skip becomes a **panic**, i.e. a
//!   failing test. An environment that is *supposed* to run the live paths sets it and can no longer
//!   pass by accident. Both venv-provisioning CI jobs set it; see `.github/workflows/ci.yml`.
//! * **Uniform, greppable marker** — `SKIPPED (live)`, so when output *is* shown the reason is
//!   consistent and searchable.
//!
//! ### What this does *not* do
//!
//! It does not make skips visible in a default `cargo test` run. The libtest harness captures a
//! passing test's stdout/stderr and prints it only on failure, so the marker surfaces under
//! `cargo test -- --nocapture` (or on a failure) and nowhere else. There is no portable way for a
//! passing test to write to the terminal.
//!
//! So the guarantee here is **not** "you will notice a skip locally" — it is "an environment that
//! claims to run the live paths cannot silently fail to". `RIPTIDE_REQUIRE_LIVE=1` in CI is the
//! enforcement; the marker is a diagnostic for when you go looking.

/// The env var that turns a live-scenario skip into a hard failure.
pub const REQUIRE_LIVE: &str = "RIPTIDE_REQUIRE_LIVE";

/// Whether the caller's environment demands that live scenarios actually run.
pub fn live_required() -> bool {
    std::env::var(REQUIRE_LIVE)
        .map(|v| v == "1")
        .unwrap_or(false)
}

/// Report that a live scenario cannot run, and say why.
///
/// Panics when [`REQUIRE_LIVE`] is set (the environment promised an interpreter and did not deliver
/// one — that is a broken environment, not an absent one); otherwise emits the `SKIPPED (live)`
/// marker (visible under `--nocapture`; see the module docs) and returns so the caller can `return`
/// out of the test.
///
/// ```ignore
/// let Some(python) = venv_python() else {
///     skip_live("`.riptide-fx-venv` (CPython 3.14) not present");
///     return;
/// };
/// ```
pub fn skip_live(reason: &str) {
    if live_required() {
        panic!(
            "live scenario unavailable: {reason}\n\
             {REQUIRE_LIVE}=1 is set, so this is a failure rather than a skip: this environment is \
             expected to run the live (real-Python) paths. Provision the interpreter, or unset \
             {REQUIRE_LIVE} to allow skipping."
        );
    }
    eprintln!(
        "SKIPPED (live): {reason} — this scenario did NOT run. \
         Set {REQUIRE_LIVE}=1 to make this a failure."
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The default posture: absent interpreter ⇒ skip, and the test binary keeps going.
    #[test]
    fn skip_is_permitted_when_live_is_not_required() {
        temp_env(None, || skip_live("no interpreter"));
    }

    /// The posture that closes the hole: an environment that promised Python must produce it.
    #[test]
    fn skip_panics_when_live_is_required() {
        let panicked =
            std::panic::catch_unwind(|| temp_env(Some("1"), || skip_live("no interpreter")));
        assert!(
            panicked.is_err(),
            "with {REQUIRE_LIVE}=1 a skipped live scenario must fail, not pass silently"
        );
    }

    /// Only an explicit `1` is strict — an unset-adjacent value must not surprise a contributor.
    #[test]
    fn other_values_do_not_enable_strict_mode() {
        temp_env(Some("0"), || assert!(!live_required()));
        temp_env(Some("true"), || assert!(!live_required()));
    }

    /// Set/restore `REQUIRE_LIVE` around `f`. These tests are the only readers of this var, but the
    /// harness threads one process, so serialize them on a mutex rather than racing the environment.
    fn temp_env<R>(value: Option<&str>, f: impl FnOnce() -> R) -> R {
        use std::sync::Mutex;
        static LOCK: Mutex<()> = Mutex::new(());
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prior = std::env::var(REQUIRE_LIVE).ok();
        // SAFETY: single-threaded within the lock; no other code in this crate reads the var.
        unsafe {
            match value {
                Some(v) => std::env::set_var(REQUIRE_LIVE, v),
                None => std::env::remove_var(REQUIRE_LIVE),
            }
        }
        let out = f();
        unsafe {
            match prior {
                Some(v) => std::env::set_var(REQUIRE_LIVE, v),
                None => std::env::remove_var(REQUIRE_LIVE),
            }
        }
        out
    }
}
