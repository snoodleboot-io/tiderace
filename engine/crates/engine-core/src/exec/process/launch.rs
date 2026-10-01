use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::error::{EngineError, Result};
use crate::exec::process::ShimProcess;

/// The interpreter and shim a run drives, and the suite root they are pointed at — the three
/// values every tier was threading as positional arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShimTarget {
    pub python: String,
    pub shim: PathBuf,
    pub root: PathBuf,
}

impl ShimTarget {
    pub fn new(python: impl Into<String>, shim: &Path, root: &Path) -> Self {
        Self {
            python: python.into(),
            shim: shim.to_path_buf(),
            root: root.to_path_buf(),
        }
    }
}

/// What the shim is launched to do: its argv, and which standard streams the engine holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShimMode {
    /// One test per exchange over the pipes; the shim forks a child per test, or runs it
    /// in-process under snapshot/restore when `restore` is on (the optimistic ladder).
    Serve { restore: bool },
    /// `--no-fork --restore`: the no-COW tier. Restore is this tier's only isolation, so it is
    /// never optional here (TID-33).
    NoFork,
    /// `--subinterp`: a pool of sub-interpreters in one process, `pool` of them or the shim's
    /// default (the CPU count).
    SubInterp { pool: Option<usize> },
    /// `--pool N --connect <socket>`: import once, fork `size` workers that dial back to
    /// `connect`. The protocol runs over the sockets, so the shim's stdout is left to the
    /// terminal and its stdin is closed.
    Pool {
        size: usize,
        connect: PathBuf,
        restore: bool,
    },
    /// `--pool 0 --connect -`: a persistent image that forks workers on request over its pipes
    /// (TID-84); each `spawn` request carries its own socket.
    PersistentPool { restore: bool },
    /// `--probe`: classify modules for the sub-interpreter tier; no tests run.
    Probe,
}

impl ShimMode {
    /// The mode's name in an error message.
    pub fn name(&self) -> &'static str {
        match self {
            ShimMode::Serve { .. } => "wellspring",
            ShimMode::NoFork => "no-fork worker",
            ShimMode::SubInterp { .. } => "sub-interpreter pool",
            ShimMode::Pool { .. } => "wellspring pool",
            ShimMode::PersistentPool { .. } => "warm pool parent",
            ShimMode::Probe => "probe",
        }
    }

    fn args(&self) -> Vec<OsString> {
        let mut out: Vec<OsString> = Vec::new();
        match self {
            ShimMode::Serve { restore } => {
                if *restore {
                    out.push("--restore".into());
                }
            }
            ShimMode::NoFork => {
                out.push("--no-fork".into());
                out.push("--restore".into());
            }
            ShimMode::SubInterp { pool } => {
                out.push("--subinterp".into());
                if let Some(n) = pool {
                    out.push("--pool-size".into());
                    out.push(n.to_string().into());
                }
            }
            ShimMode::Pool {
                size,
                connect,
                restore,
            } => {
                out.push("--pool".into());
                out.push(size.to_string().into());
                out.push("--connect".into());
                out.push(connect.clone().into_os_string());
                if *restore {
                    out.push("--restore".into());
                }
            }
            ShimMode::PersistentPool { restore } => {
                out.push("--pool".into());
                out.push("0".into());
                out.push("--connect".into());
                out.push("-".into());
                if *restore {
                    out.push("--restore".into());
                }
            }
            ShimMode::Probe => out.push("--probe".into()),
        }
        out
    }

    /// Whether the engine speaks to this process over its stdin/stdout.
    fn piped(&self) -> bool {
        !matches!(self, ShimMode::Pool { .. })
    }
}

/// A shim launch: `python <shim> <root> <mode args> [--modules <file>]`, with the native
/// thread pools pinned to one thread — threaded BLAS/OMP plus `fork()` is a known hazard, and
/// parallel workers must not oversubscribe them either.
#[derive(Debug, Clone)]
pub struct ShimLaunch<'a> {
    target: &'a ShimTarget,
    mode: ShimMode,
    modules: Option<&'a Path>,
}

impl<'a> ShimLaunch<'a> {
    pub fn new(target: &'a ShimTarget, mode: ShimMode) -> Self {
        Self {
            target,
            mode,
            modules: None,
        }
    }

    /// Import only the modules named in `file` — one suite-relative path per line — and the
    /// conftests above them, instead of the whole suite (TID-75).
    pub fn modules(mut self, file: Option<&'a Path>) -> Self {
        self.modules = file;
        self
    }

    /// Spawn the process. The readiness handshake is the caller's: it comes over the pipes,
    /// which the caller may first wrap in a transport.
    pub fn spawn(self) -> Result<ShimProcess> {
        let mut cmd = Command::new(&self.target.python);
        cmd.arg(&self.target.shim)
            .arg(&self.target.root)
            .args(self.mode.args());
        if let Some(file) = self.modules {
            cmd.arg("--modules").arg(file);
        }
        cmd.env("OPENBLAS_NUM_THREADS", "1")
            .env("OMP_NUM_THREADS", "1")
            .env("MKL_NUM_THREADS", "1");
        if self.mode.piped() {
            cmd.stdin(Stdio::piped()).stdout(Stdio::piped());
        } else {
            cmd.stdin(Stdio::null());
        }
        let child = cmd.spawn().map_err(|e| {
            EngineError::Exec(format!("failed to launch the {}: {e}", self.mode.name()))
        })?;
        Ok(ShimProcess::new(child, self.mode))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_mode_has_the_argv_the_shim_dispatches_on() {
        let args = |m: ShimMode| -> Vec<String> {
            m.args()
                .into_iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect()
        };
        assert!(args(ShimMode::Serve { restore: false }).is_empty());
        assert_eq!(args(ShimMode::Serve { restore: true }), ["--restore"]);
        assert_eq!(args(ShimMode::NoFork), ["--no-fork", "--restore"]);
        assert_eq!(args(ShimMode::SubInterp { pool: None }), ["--subinterp"]);
        assert_eq!(
            args(ShimMode::SubInterp { pool: Some(3) }),
            ["--subinterp", "--pool-size", "3"]
        );
        assert_eq!(
            args(ShimMode::Pool {
                size: 4,
                connect: PathBuf::from("/tmp/s"),
                restore: true
            }),
            ["--pool", "4", "--connect", "/tmp/s", "--restore"]
        );
        assert_eq!(
            args(ShimMode::PersistentPool { restore: false }),
            ["--pool", "0", "--connect", "-"]
        );
        assert_eq!(args(ShimMode::Probe), ["--probe"]);
    }

    #[test]
    fn a_missing_interpreter_is_a_typed_launch_error() {
        let target = ShimTarget::new(
            "definitely-not-a-python-xyz",
            Path::new("shim.py"),
            Path::new("."),
        );
        let err = ShimLaunch::new(&target, ShimMode::Probe)
            .spawn()
            .expect_err("no such interpreter");
        assert!(
            err.to_string().contains("failed to launch the probe"),
            "{err}"
        );
    }
}
