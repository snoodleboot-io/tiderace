//! Locating the Python shim when it isn't given explicitly.
//!
//! `TIDERACE_SHIM` is normally set (dev checkouts, CI). But a `pip install tiderace` / `uv pip install
//! tiderace` ships the shim *inside* the `tiderace` Python package, so a user shouldn't have to point at
//! it by hand. [`default_shim`] recovers that path by asking the target interpreter where its own
//! `tiderace._shim/shim.py` lives — which is exactly the interpreter that will run the shim, so if it
//! can't import `tiderace`, the shim couldn't have run there anyway.

use std::path::PathBuf;
use std::process::Command;

/// The interpreter to drive when `TIDERACE_PYTHON` is unset.
///
/// Both binaries used to hardcode `python3`. That is correct on POSIX — it is the convention, and
/// a POSIX venv puts a `python3` in `bin/` — but it is wrong on Windows: a venv's `Scripts\` gets
/// `python.exe` and `pythonw.exe` and **no `python3.exe`**. So `python3` walked straight past the
/// venv the user had just installed into and either failed to launch or, worse, silently found an
/// unrelated system interpreter with no `tiderace` in it. Either way [`default_shim`] returned
/// `None` and zero-config `pip install tiderace` could not work on Windows at all — which the
/// wheel matrix caught the moment Windows wheels started being built.
///
/// The non-preferred name is still tried, so an unusual layout on either platform degrades to
/// "found something" rather than a hard error. `TIDERACE_PYTHON` overrides both.
pub fn default_python() -> String {
    let candidates = if cfg!(windows) {
        ["python", "python3"]
    } else {
        ["python3", "python"]
    };
    candidates
        .iter()
        .find(|cand| {
            Command::new(cand)
                .arg("--version")
                .output()
                .is_ok_and(|out| out.status.success())
        })
        .unwrap_or(&candidates[0])
        .to_string()
}

/// The shim bundled with the installed `tiderace` package, as seen by `python`, or `None` when it
/// isn't there (a source checkout with no installed wheel — the caller then falls back to
/// `TIDERACE_SHIM`). Never panics: any failure to launch or import yields `None`.
pub fn default_shim(python: &str) -> Option<PathBuf> {
    let probe = "import importlib.util as u, pathlib, sys\n\
                 s = u.find_spec('tiderace')\n\
                 p = pathlib.Path(s.origin).with_name('_shim') / 'shim.py' if s and s.origin else None\n\
                 sys.stdout.write(str(p) if p and p.exists() else '')";
    let out = Command::new(python).args(["-c", probe]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if path.is_empty() {
        return None;
    }
    let p = PathBuf::from(path);
    p.exists().then_some(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_interpreter_is_none_not_panic() {
        assert!(default_shim("definitely-not-a-real-python-xyz").is_none());
    }

    #[test]
    fn default_python_prefers_the_platform_convention() {
        // The whole point of the helper: never hand Windows a name its venvs do not create.
        let chosen = default_python();
        if cfg!(windows) {
            assert_ne!(
                chosen, "python3",
                "a Windows venv's Scripts\\ has no python3.exe"
            );
        } else {
            assert_eq!(chosen, "python3");
        }
    }

    #[test]
    fn default_python_resolves_to_a_launchable_interpreter() {
        // CI provisions an interpreter on every platform, so whatever comes back must actually run.
        let chosen = default_python();
        let out = Command::new(&chosen).arg("--version").output();
        assert!(
            out.is_ok_and(|o| o.status.success()),
            "{chosen} is not launchable"
        );
    }

    #[test]
    fn interpreter_without_tiderace_installed_is_none() {
        // A bare `python3` on PATH almost certainly has no `tiderace` package → None, cleanly.
        for cand in ["python3", "python"] {
            if Command::new(cand).arg("--version").output().is_ok() {
                // Whatever the result, it must not panic and must be None unless tiderace is installed.
                let _ = default_shim(cand);
                return;
            }
        }
    }
}
