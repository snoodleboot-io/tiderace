//! `tiderace run --etf <path>`: the run's per-test traces in the Estuary Trace Format (TID-128).
//!
//! estuary reuses a test's result on any snapshot where everything the test read is unchanged,
//! and ETF is how a harness tells it what that was. The file is written by the shim, through
//! the `estuary_etf` Python binding — one codec, shared with the store — so this side only
//! arranges the run and hands the results over:
//!
//!   1. before anything runs, ask the interpreter whether the binding is there (`--etf-check`),
//!      so a missing optional dependency is one clear line and not a suite's worth of waiting;
//!   2. turn footprint capture on for the workers, over the *repository* rather than the run
//!      root — `tiderace run tests/` must still name the source beside `tests/`;
//!   3. after the report, write a manifest of what ran and launch the shim's `--etf-emit` mode.
//!
//! Without `--etf` none of this exists: no environment is set, nothing is spawned.

use std::path::{Component, Path, PathBuf};
use std::process::Command;

use engine_core::domain::TestResult;
use serde_json::json;

/// The shim's exit status for "the binding is not installed" (`tiderace_shim.etf.EXIT_NO_BINDING`).
const EXIT_NO_BINDING: i32 = 3;

/// One run's ETF output: where it goes and the tree its keys are relative to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EtfOutput {
    pub out: PathBuf,
    pub run_root: PathBuf,
    pub trace_root: PathBuf,
    sys_path_file: PathBuf,
}

impl EtfOutput {
    /// Where the trace goes and what its keys are relative to: `explicit` (`--etf-root`) when
    /// given, else the nearest directory at or above the run root that holds `.git`, else the
    /// run root itself. An explicit root that does not contain the run root is refused — every
    /// key would be outside it.
    pub fn plan(out: &Path, explicit: Option<&Path>, run_root: &Path) -> Result<Self, String> {
        let run_root = absolute(run_root)?;
        let trace_root = match explicit {
            Some(dir) => {
                let dir = absolute(dir)?;
                if !run_root.starts_with(&dir) {
                    return Err(format!(
                        "--etf-root {} does not contain the run root {}",
                        dir.display(),
                        run_root.display()
                    ));
                }
                dir
            }
            None => run_root
                .ancestors()
                .find(|dir| dir.join(".git").exists())
                .unwrap_or(&run_root)
                .to_path_buf(),
        };
        let sys_path_file =
            std::env::temp_dir().join(format!("tiderace-etf-{}-syspath.json", std::process::id()));
        Ok(Self {
            out: absolute(out)?,
            run_root,
            trace_root,
            sys_path_file,
        })
    }

    /// The environment the workers read, as `(name, value)`: footprint capture on, where to
    /// leave their import roots, and — only when the trace covers more than the run root — the
    /// tree footprints are taken over.
    pub fn worker_env(&self) -> Vec<(&'static str, PathBuf)> {
        let mut env = vec![
            ("TIDERACE_COVERAGE", PathBuf::from("1")),
            ("TIDERACE_ETF_SYSPATH", self.sys_path_file.clone()),
        ];
        if self.trace_root != self.run_root {
            env.push(("TIDERACE_FOOTPRINT_ROOT", self.trace_root.clone()));
        }
        env
    }

    /// Whether `python` can import the binding. `Err` carries the shim's own message, which
    /// names the interpreter and the install command.
    pub fn preflight(&self, python: &str, shim: &Path) -> Result<(), String> {
        let out = Command::new(python)
            .arg(shim)
            .arg(&self.run_root)
            .arg("--etf-check")
            .output()
            .map_err(|e| format!("--etf: could not launch {python}: {e}"))?;
        if out.status.success() {
            return Ok(());
        }
        Err(relayed(&out.stderr, out.status.code()))
    }

    /// The manifest the shim's `--etf-emit` mode reads: the roots, the header's fields, and
    /// per node its id, outcome, duration and footprint.
    pub fn manifest(&self, results: &[TestResult]) -> serde_json::Value {
        let tests: Vec<serde_json::Value> = results
            .iter()
            .map(|r| {
                json!({
                    "id": r.node_id.as_str(),
                    "outcome": r.outcome.token(),
                    "duration_ms": r.duration_ms,
                    "reads": { "file": r.touched_files },
                })
            })
            .collect();
        json!({
            "v": 1,
            "out": self.out,
            "run_root": self.run_root,
            "trace_root": self.trace_root,
            "engine_version": env!("CARGO_PKG_VERSION"),
            "created_at_ms": created_at_ms(),
            "tree": std::env::var("TIDERACE_ETF_TREE").ok(),
            "sys_path_file": self.sys_path_file,
            "tests": tests,
        })
    }

    /// Write the file. `Ok` is the emitter's one-line summary.
    pub fn emit(
        &self,
        python: &str,
        shim: &Path,
        results: &[TestResult],
    ) -> Result<String, String> {
        let manifest_path =
            std::env::temp_dir().join(format!("tiderace-etf-{}-manifest.json", std::process::id()));
        let written = std::fs::write(&manifest_path, self.manifest(results).to_string())
            .map_err(|e| format!("--etf: could not write {}: {e}", manifest_path.display()));
        let out = written.and_then(|()| {
            Command::new(python)
                .arg(shim)
                .arg(&self.run_root)
                .arg("--etf-emit")
                .arg(&manifest_path)
                .output()
                .map_err(|e| format!("--etf: could not launch {python}: {e}"))
        });
        let _ = std::fs::remove_file(&manifest_path);
        let _ = std::fs::remove_file(&self.sys_path_file);
        let out = out?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
        } else {
            Err(relayed(&out.stderr, out.status.code()))
        }
    }
}

/// The shim's last line on stderr, without its `tiderace: ` prefix — the caller adds its own.
fn relayed(stderr: &[u8], code: Option<i32>) -> String {
    let text = String::from_utf8_lossy(stderr);
    let line = text.lines().rev().find(|l| !l.trim().is_empty());
    match line {
        Some(line) => line.trim_start_matches("tiderace: ").to_string(),
        None if code == Some(EXIT_NO_BINDING) => {
            "--etf: the estuary_etf module is not installed".to_string()
        }
        None => format!("--etf: the emitter failed ({code:?}) and said nothing"),
    }
}

/// `SOURCE_DATE_EPOCH` when set, so a build that pins it gets the same bytes twice; else now.
fn created_at_ms() -> u64 {
    if let Some(secs) = std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
    {
        return secs.saturating_mul(1000);
    }
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// `path` made absolute and lexically normal — `.` dropped, `..` folded — without resolving
/// symlinks: what the shim's `os.path.abspath` gives, so the two sides name one directory alike.
fn absolute(path: &Path) -> Result<PathBuf, String> {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| format!("--etf: no working directory: {e}"))?
            .join(path)
    };
    let mut out = PathBuf::new();
    for part in joined.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_core::domain::{NodeId, Outcome};

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tiderace_etf_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("repo/pkg/tests")).unwrap();
        dir
    }

    #[test]
    fn the_trace_root_is_the_repository_not_the_run_root() {
        let dir = scratch("root");
        let repo = dir.join("repo");
        let tests = repo.join("pkg/tests");

        // No `.git` anywhere: the run root is all there is to be relative to.
        let plain = EtfOutput::plan(Path::new("out.etf"), None, &tests).unwrap();
        assert_eq!(plain.trace_root, tests);
        assert!(
            plain.out.is_absolute(),
            "the emitter runs elsewhere: {:?}",
            plain.out
        );
        assert!(
            !plain
                .worker_env()
                .iter()
                .any(|(k, _)| *k == "TIDERACE_FOOTPRINT_ROOT"),
            "same tree as the run root: the workers are told nothing new"
        );

        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let found = EtfOutput::plan(Path::new("out.etf"), None, &tests.join("../tests/.")).unwrap();
        assert_eq!(found.run_root, tests, "normalised as os.path.abspath would");
        assert_eq!(found.trace_root, repo);
        let env = found.worker_env();
        assert!(env.contains(&("TIDERACE_COVERAGE", PathBuf::from("1"))));
        assert!(env.contains(&("TIDERACE_FOOTPRINT_ROOT", repo.clone())));

        let explicit =
            EtfOutput::plan(Path::new("out.etf"), Some(&repo.join("pkg")), &tests).unwrap();
        assert_eq!(explicit.trace_root, repo.join("pkg"));
        let outside = EtfOutput::plan(Path::new("out.etf"), Some(&dir.join("elsewhere")), &tests);
        assert!(
            outside
                .unwrap_err()
                .contains("does not contain the run root"),
            "a root the run is not under would put every key outside it"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_manifest_carries_each_node_with_its_footprint() {
        let dir = scratch("manifest");
        let etf = EtfOutput::plan(&dir.join("out.etf"), None, &dir.join("repo")).unwrap();
        let results = vec![
            TestResult::new(
                NodeId::new("tests/test_a.py::test_one"),
                Outcome::Passed,
                7,
                "",
            )
            .with_touched(vec!["src/a.py".into(), "tests/test_a.py".into()]),
            TestResult::new(
                NodeId::new("tests/test_a.py::test_two"),
                Outcome::XFail,
                0,
                "why",
            ),
        ];
        let m = etf.manifest(&results);
        assert_eq!(m["v"], 1);
        assert_eq!(m["engine_version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(m["tests"][0]["id"], "tests/test_a.py::test_one");
        assert_eq!(m["tests"][0]["outcome"], "passed");
        assert_eq!(m["tests"][0]["duration_ms"], 7);
        assert_eq!(m["tests"][0]["reads"]["file"][0], "src/a.py");
        assert_eq!(m["tests"][1]["outcome"], "xfail");
        assert!(m["created_at_ms"].as_u64().is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_emitter_is_relayed_in_its_own_words() {
        assert_eq!(
            relayed(
                b"noise\ntiderace: --etf writes its output through ...\n\n",
                Some(3)
            ),
            "--etf writes its output through ..."
        );
        assert!(relayed(b"", Some(3)).contains("estuary_etf module is not installed"));
        assert!(relayed(b"", Some(1)).contains("said nothing"));
    }
}
