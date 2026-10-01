//! Test-support for **live scenarios** — the ones that need a real Python (`.tiderace-fx-venv`, a
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
//! That is not hypothetical. `.tiderace-fx-venv/bin/python` was a symlink into a *versioned* VSCode
//! snap path (`snap/code/244/…`); when that revision was garbage-collected the venv broke, and
//! `cargo test --workspace` kept reporting `ok` with **10 live tests silently skipped**.
//!
//! ## The contract
//!
//! Call [`skip_live`] instead of `eprintln!("SKIP: …")`.
//!
//! * **Strict where it counts** — with `TIDERACE_REQUIRE_LIVE=1` a skip becomes a **panic**, i.e. a
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
//! claims to run the live paths cannot silently fail to". `TIDERACE_REQUIRE_LIVE=1` in CI is the
//! enforcement; the marker is a diagnostic for when you go looking.

/// The env var that turns a live-scenario skip into a hard failure.
pub const REQUIRE_LIVE: &str = "TIDERACE_REQUIRE_LIVE";

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
///     skip_live("`.tiderace-fx-venv` (CPython 3.14) not present");
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

// ---------------------------------------------------------------------------------------------
// Paths, interpreters and scratch directories for the acceptance tests (TID-107).
//
// Before this every acceptance test re-declared its own `repo_root()`, `shim()`, a Python finder
// and a `scratch()` — 80, 77, 49 and 32 copies, the finders in four diverging bodies and the
// scratch helpers in thirty-one. This is the one spelling of each.
// ---------------------------------------------------------------------------------------------

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

/// The checkout's root — three up from this crate's manifest.
pub fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repo root")
}

/// The canonical shim, `engine/py-shim/shim.py` — what `TIDERACE_SHIM` points at in a checkout.
pub fn shim() -> PathBuf {
    repo_root().join("engine/py-shim/shim.py")
}

/// Where the fx venv's interpreter lives, relative to the repo root: CPython 3.14 with pytest,
/// numpy, `concurrent.interpreters` and `engine/py-tiderace` importable. CI provisions it; a
/// fresh clone has none.
pub const FX_VENV_PYTHON: &str = ".tiderace-fx-venv/bin/python";

/// The fx venv's interpreter, when present.
pub fn fx_venv_python() -> Option<String> {
    let p = repo_root().join(FX_VENV_PYTHON);
    p.exists().then(|| p.to_string_lossy().into_owned())
}

/// What a live scenario needs from its interpreter. The finder probes each candidate for it, so
/// a test skips with the reason named rather than failing on an `ImportError` three layers down.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PythonNeeds {
    /// Any interpreter that starts.
    Any,
    /// `import pytest` works.
    Pytest,
    /// `import tiderace.builtins` works (the `engine/py-tiderace` package).
    Tiderace,
    /// Both of the above — the builtins providers under a pytest-style suite.
    PytestAndTiderace,
    /// pytest and `concurrent.interpreters` (CPython 3.14+): the sub-interpreter tier.
    SubInterpreters,
    /// The fx venv itself, and nothing else: scenarios pinned to its exact package set.
    FxVenv,
}

impl PythonNeeds {
    fn probe(self) -> &'static str {
        match self {
            PythonNeeds::Any | PythonNeeds::FxVenv => "import sys",
            PythonNeeds::Pytest => "import pytest",
            PythonNeeds::Tiderace => "import tiderace.builtins",
            PythonNeeds::PytestAndTiderace => "import pytest, tiderace.builtins",
            PythonNeeds::SubInterpreters => "import pytest, concurrent.interpreters",
        }
    }

    /// The need, as a skip message names it.
    pub fn describe(self) -> &'static str {
        match self {
            PythonNeeds::Any => "a Python interpreter",
            PythonNeeds::Pytest => "an interpreter with pytest",
            PythonNeeds::Tiderace => "an interpreter with tiderace importable",
            PythonNeeds::PytestAndTiderace => "an interpreter with pytest and tiderace importable",
            PythonNeeds::SubInterpreters => {
                "an interpreter with pytest and concurrent.interpreters (3.14+)"
            }
            PythonNeeds::FxVenv => "`.tiderace-fx-venv` (CPython 3.14)",
        }
    }
}

fn can_import(python: &str, statement: &str) -> bool {
    Command::new(python)
        .args(["-c", statement])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// An interpreter that satisfies `needs`: the fx venv first, then `python3`, then `python`.
/// `None` when nothing on this machine does — pair with [`skip_live`], or use
/// [`require_python`], which does that for you.
pub fn python(needs: PythonNeeds) -> Option<String> {
    if needs == PythonNeeds::FxVenv {
        return fx_venv_python();
    }
    let mut cands: Vec<String> = fx_venv_python().into_iter().collect();
    cands.extend(["python3".to_string(), "python".to_string()]);
    cands.into_iter().find(|p| can_import(p, needs.probe()))
}

/// [`python`], and on `None` the [`skip_live`] call with the need named — so a live test is
/// `let Some(python) = require_python(PythonNeeds::Pytest) else { return };`.
pub fn require_python(needs: PythonNeeds) -> Option<String> {
    let found = python(needs);
    if found.is_none() {
        skip_live(&format!("no interpreter found: needs {}", needs.describe()));
    }
    found
}

/// A fresh, empty directory under the system temp dir, unique per call within and across test
/// processes: `tiderace_<tag>_<pid>_<n>`. The caller removes it when done (or leaves it for a
/// post-mortem — nothing here depends on cleanup).
pub fn scratch(tag: &str) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "tiderace_{tag}_{}_{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

use crate::domain::Outcome;
use crate::error::{EngineError, Result};
use crate::exec::{ExecRequest, ExecResponse, ReadyInfo, ShimTransport};

/// A pure-Rust shim that answers from a script — **no process, no pipe, no syscall**. Proves a
/// run loop (request build → exchange → `TestResult` assembly) end to end, offline. Public so
/// every worker's loop can be tested this way, not only `run_batch`.
pub struct ScriptedShim {
    pid: Option<u32>,
    /// node_id → (outcome wire token, detail).
    script: std::collections::HashMap<String, (String, String)>,
    /// Outcome for any node_id not in `script`.
    default_outcome: String,
    /// node_ids in the order they were asked — lets a test assert on dispatch order.
    seen: std::vec::Vec<String>,
    /// If set, the Nth (0-based) exchange and every one after fails as if the shim closed mid-run.
    close_after: Option<usize>,
    calls: usize,
}

impl Default for ScriptedShim {
    fn default() -> Self {
        Self::new()
    }
}

impl ScriptedShim {
    /// The node ids asked so far, in order.
    pub fn seen(&self) -> &[String] {
        &self.seen
    }

    pub fn new() -> Self {
        Self {
            pid: None,
            script: std::collections::HashMap::new(),
            default_outcome: "passed".into(),
            seen: Vec::new(),
            close_after: None,
            calls: 0,
        }
    }

    pub fn answer(mut self, node_id: &str, outcome: &str, detail: &str) -> Self {
        self.script
            .insert(node_id.into(), (outcome.into(), detail.into()));
        self
    }

    pub fn closes_after(mut self, n: usize) -> Self {
        self.close_after = Some(n);
        self
    }
}

impl ShimTransport for ScriptedShim {
    fn ready(&mut self) -> Result<ReadyInfo> {
        Ok(ReadyInfo { pid: self.pid })
    }

    fn exchange(&mut self, req: &ExecRequest<'_>) -> Result<ExecResponse> {
        if matches!(self.close_after, Some(n) if self.calls >= n) {
            return Err(EngineError::PeerClosed { what: "shim" });
        }
        self.calls += 1;
        self.seen.push(req.node_id.to_string());
        let (outcome, detail) = self
            .script
            .get(req.node_id.as_str())
            .cloned()
            .unwrap_or((self.default_outcome.clone(), String::new()));
        // Through serde, as the wire does: a token the engine does not know reads as `Error`.
        let outcome: Outcome =
            serde_json::from_value(serde_json::Value::String(outcome)).unwrap_or(Outcome::Error);
        Ok(ExecResponse {
            must_fork: false,
            node_id: req.node_id.clone(),
            outcome,
            detail,
            coverage: Default::default(),
            pure: None,
            skip_origin: None,
            keywords: Vec::new(),
            variants: Vec::new(),
            expanded: false,
        })
    }
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

    #[test]
    fn scratch_dirs_are_unique_and_empty() {
        let a = scratch("t107");
        let b = scratch("t107");
        assert_ne!(a, b);
        assert!(a.is_dir() && b.is_dir());
        assert_eq!(std::fs::read_dir(&a).unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(&a);
        let _ = std::fs::remove_dir_all(&b);
    }

    #[test]
    fn repo_root_holds_the_shim() {
        assert!(shim().is_file(), "{}", shim().display());
        assert!(repo_root().join("engine/crates").is_dir());
    }

    #[test]
    fn an_impossible_need_is_none_not_a_panic() {
        // A probe nothing satisfies: the finder returns None and the caller skips.
        assert!(!can_import("definitely-not-a-python-xyz", "import sys"));
    }
}
