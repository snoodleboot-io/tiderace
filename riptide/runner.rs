use anyhow::{Context, Result};
use colored::Colorize;
use rayon::prelude::*;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use wait_timeout::ChildExt;

use crate::collector::TestItem;

/// Cap on per-test captured stdout/stderr persisted to the database, to bound
/// memory and DB growth from runaway test output (security finding #4).
const MAX_CAPTURE_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub enum TestStatus {
    Passed,
    Failed,
    Error,
    Skipped,
}

impl TestStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            TestStatus::Passed => "passed",
            TestStatus::Failed => "failed",
            TestStatus::Error => "error",
            TestStatus::Skipped => "skipped",
        }
    }
}

#[derive(Debug, Clone)]
pub struct TestResult {
    pub test_id: String,
    pub file_path: String,
    pub status: TestStatus,
    pub duration_ms: i64,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
    /// Files touched during this test run (from coverage)
    pub covered_files: Vec<String>,
}

pub struct Runner {
    pub workers: usize,
    pub python_bin: String,
    pub with_coverage: bool,
    pub coverage_dir: PathBuf,
    /// Per-test (isolated) or per-batch (batched) wall-clock limit; on expiry the
    /// child is killed and the affected test(s) recorded as Error.
    pub timeout: Duration,
    /// Force the legacy one-process-per-test path even without coverage.
    pub isolate: bool,
}

impl Runner {
    pub fn new(
        workers: usize,
        python_bin: &str,
        with_coverage: bool,
        timeout_secs: u64,
        isolate: bool,
    ) -> Self {
        Runner {
            workers,
            python_bin: python_bin.to_string(),
            with_coverage,
            coverage_dir: PathBuf::from(".riptide-coverage"),
            timeout: Duration::from_secs(timeout_secs),
            isolate,
        }
    }

    /// Run all selected tests in parallel using a scoped rayon pool.
    ///
    /// Two execution strategies (ADR-009):
    ///   * **batched** (default) — distribute tests across the pool and run ONE
    ///     pytest process per worker, amortising interpreter startup. Per-test
    ///     outcomes are recovered from pytest's `-rA` summary.
    ///   * **isolated** — one pytest process per test. Used when coverage is on
    ///     (to record a precise per-test dependency graph) or `--isolate` is set.
    ///
    /// Either way, `par_iter().map().collect()` avoids the shared `Mutex<Vec<_>>`
    /// and its lock-poisoning panics, and the worker count is honored via a scoped
    /// pool.
    pub fn run_parallel(&self, tests: &[TestItem]) -> Result<Vec<TestResult>> {
        if self.with_coverage {
            std::fs::create_dir_all(&self.coverage_dir)?;
        }

        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(self.workers)
            .build()
            .context("failed to build worker thread pool")?;

        let total = tests.len();
        let counter = AtomicUsize::new(0);

        if self.with_coverage || self.isolate {
            return Ok(pool.install(|| {
                tests
                    .par_iter()
                    .map(|test| {
                        let result = self.run_single(test);
                        let n = counter.fetch_add(1, Ordering::SeqCst) + 1;
                        match &result {
                            Ok(r) => print_progress(n, total, r),
                            Err(e) => {
                                eprintln!("  {} [ERROR] {}: {}", "✗".red(), test.test_id, e)
                            }
                        }
                        result.ok()
                    })
                    .flatten()
                    .collect()
            }));
        }

        // Batched: split into one chunk per worker (each chunk = one pytest run).
        let chunk_size = tests.len().div_ceil(self.workers.max(1)).max(1);
        let chunks: Vec<&[TestItem]> = tests.chunks(chunk_size).collect();
        let results: Vec<TestResult> = pool.install(|| {
            chunks
                .par_iter()
                .map(|chunk| self.run_chunk(chunk, total, &counter))
                .reduce(Vec::new, |mut acc, mut v| {
                    acc.append(&mut v);
                    acc
                })
        });
        Ok(results)
    }

    /// Run one chunk of tests in a single pytest process and recover per-test
    /// outcomes from the `-rA` summary. Any test the summary does not mention
    /// (e.g. a collection error took down the batch) is recorded as an Error.
    fn run_chunk(
        &self,
        chunk: &[TestItem],
        total: usize,
        counter: &AtomicUsize,
    ) -> Vec<TestResult> {
        let node_ids: Vec<String> = chunk.iter().map(|t| t.pytest_nodeid()).collect();
        let out_path = unique_temp("batch.out");

        let statuses = match self.exec_chunk(&node_ids, &out_path) {
            Ok(map) => map,
            Err(e) => {
                eprintln!("  {} [BATCH ERROR] {}", "✗".red(), e);
                HashMap::new()
            }
        };
        let _ = std::fs::remove_file(&out_path);

        chunk
            .iter()
            .map(|test| {
                let node_id = test.pytest_nodeid();
                // A test missing from the summary did not report an outcome.
                let status = statuses.get(&node_id).cloned().unwrap_or(TestStatus::Error);
                let result = TestResult {
                    test_id: test.test_id.clone(),
                    file_path: test.file_path.clone(),
                    status,
                    duration_ms: 0, // per-test timing is not available in batch mode
                    stdout: None,
                    stderr: None,
                    covered_files: Vec::new(),
                };
                let n = counter.fetch_add(1, Ordering::SeqCst) + 1;
                print_progress(n, total, &result);
                result
            })
            .collect()
    }

    /// Spawn the batched pytest process and parse its `-rA` summary into a
    /// node-id -> status map.
    fn exec_chunk(
        &self,
        node_ids: &[String],
        out_path: &Path,
    ) -> Result<HashMap<String, TestStatus>> {
        let out_file = File::create(out_path)?;
        let mut cmd = Command::new(&self.python_bin);
        // -rA prints an outcome line per test; no -x so the whole batch runs.
        cmd.args([
            "-m",
            "pytest",
            "-rA",
            "--tb=no",
            "-q",
            "-p",
            "no:cacheprovider",
            "--",
        ]);
        cmd.args(node_ids);
        cmd.stdout(Stdio::from(out_file));
        cmd.stderr(Stdio::null());

        let mut child = cmd
            .spawn()
            .context("failed to spawn batched test process")?;
        match child.wait_timeout(self.timeout)? {
            Some(_) => {}
            None => {
                let _ = child.kill();
                let _ = child.wait();
                // Timed out: leave the map empty so every test in the batch is Error.
                return Ok(HashMap::new());
            }
        }
        Ok(parse_batch_summary(&read_capped(out_path)))
    }

    /// Run a single test in its own pytest subprocess, with a timeout and
    /// bounded, file-backed output capture.
    fn run_single(&self, test: &TestItem) -> Result<TestResult> {
        let start = Instant::now();
        let node_id = test.pytest_nodeid();

        // Collision-resistant per-test filename derived from a hash of the full
        // test id (security finding #3 / #6 — replaces the lossy char-strip).
        let safe_id = short_hash(&test.test_id);
        let cov_file = self.coverage_dir.join(format!(".coverage.{}", safe_id));
        let cov_arg = cov_file
            .to_str()
            .with_context(|| format!("coverage path is not valid UTF-8: {:?}", cov_file))?;

        // Capture child output via temp files rather than pipes: this avoids the
        // pipe-buffer deadlock that `wait_timeout` + piped stdio would hit, and
        // lets us read a bounded slice afterwards. Names are process-unique so
        // concurrent runs never share a capture file.
        let out_path = unique_temp("out");
        let err_path = unique_temp("err");
        let out_file = File::create(&out_path)?;
        let err_file = File::create(&err_path)?;

        let mut cmd = Command::new(&self.python_bin);
        cmd.arg("-m");
        if self.with_coverage {
            cmd.args([
                "coverage",
                "run",
                "--data-file",
                cov_arg,
                "--source=.",
                "--branch",
                "-m",
                "pytest",
            ]);
        } else {
            cmd.arg("pytest");
        }
        // Flags first, then `--` so the node id can never be parsed as an option
        // (security finding #1 — argument injection via crafted paths/names).
        cmd.args(["-x", "--tb=short", "-q", "--no-header", "--", &node_id]);
        cmd.stdout(Stdio::from(out_file));
        cmd.stderr(Stdio::from(err_file));

        let mut child = cmd.spawn().context("failed to spawn test subprocess")?;

        let (exit_code, timed_out) = match child.wait_timeout(self.timeout)? {
            Some(exit) => (exit.code(), false),
            None => {
                // Exceeded the limit — kill the child and reap it.
                let _ = child.kill();
                let _ = child.wait();
                (None, true)
            }
        };
        let duration_ms = start.elapsed().as_millis() as i64;

        let mut stdout = read_capped(&out_path);
        let stderr = read_capped(&err_path);
        let _ = std::fs::remove_file(&out_path);
        let _ = std::fs::remove_file(&err_path);

        let status = if timed_out {
            stdout.push_str(&format!(
                "\n[riptide] test exceeded timeout of {}s and was killed\n",
                self.timeout.as_secs()
            ));
            TestStatus::Error
        } else {
            parse_status(exit_code, looks_skipped(&stdout))
        };

        let covered_files = if self.with_coverage && cov_file.exists() {
            extract_covered_files(&self.python_bin, &cov_file).unwrap_or_default()
        } else {
            Vec::new()
        };

        Ok(TestResult {
            test_id: test.test_id.clone(),
            file_path: test.file_path.clone(),
            status,
            duration_ms,
            stdout: Some(stdout),
            stderr: if stderr.is_empty() {
                None
            } else {
                Some(stderr)
            },
            covered_files,
        })
    }
}

/// Short hex hash (first 16 bytes of SHA-256) for use in collision-resistant filenames.
fn short_hash(s: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(s.as_bytes());
    hex::encode(&hasher.finalize()[..16])
}

static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// A temp path unique to this process and call site. Concurrent riptide runs (and
/// parallel workers within one run) share the OS temp dir, so a content-derived
/// name alone can collide — the pid plus a monotonic counter make it unique.
fn unique_temp(suffix: &str) -> PathBuf {
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("riptide-{}-{}.{}", std::process::id(), seq, suffix))
}

/// Read a file's contents, truncated to [`MAX_CAPTURE_BYTES`] on a char boundary.
fn read_capped(path: &Path) -> String {
    let bytes = std::fs::read(path).unwrap_or_default();
    if bytes.len() <= MAX_CAPTURE_BYTES {
        return String::from_utf8_lossy(&bytes).into_owned();
    }
    // Slicing may land mid-codepoint; from_utf8_lossy replaces the partial tail.
    let mut s = String::from_utf8_lossy(&bytes[..MAX_CAPTURE_BYTES]).into_owned();
    s.push_str("\n[riptide] output truncated\n");
    s
}

/// Parse pytest's `-rA` summary into a node-id -> status map. Lines look like:
///   `PASSED tests/test_x.py::test_a`
///   `FAILED tests/test_x.py::test_b - assert False`
/// The first token is the outcome and the second is the exact node id (any
/// trailing ` - reason` is ignored).
fn parse_batch_summary(stdout: &str) -> HashMap<String, TestStatus> {
    let mut map = HashMap::new();
    for line in stdout.lines() {
        let mut parts = line.splitn(3, ' ');
        let outcome = match parts.next() {
            Some(o) => o,
            None => continue,
        };
        let status = match outcome {
            "PASSED" | "XPASS" => TestStatus::Passed,
            "FAILED" => TestStatus::Failed,
            "ERROR" => TestStatus::Error,
            "SKIPPED" | "XFAIL" => TestStatus::Skipped,
            _ => continue,
        };
        // The node id is the second whitespace-free token; a ` - reason` tail
        // (when present) lands in the third split and is ignored.
        if let Some(node_id) = parts.next() {
            let node_id = node_id.trim();
            if !node_id.is_empty() {
                map.insert(node_id.to_string(), status);
            }
        }
    }
    map
}

/// Heuristic for a passed-but-skipped run: pytest exits 0 for both passed and
/// skipped, so the summary line is the only discriminator.
fn looks_skipped(stdout: &str) -> bool {
    let s = stdout.to_lowercase();
    s.contains("skipped") && !s.contains(" passed") && !s.contains(" failed")
}

/// Map a pytest process exit code to a [`TestStatus`].
///
/// Far more robust than scraping stdout. pytest's exit codes are stable:
///   0 = all collected tests passed (or were skipped)
///   1 = some tests failed
///   2 = interrupted   3 = internal error   4 = usage error   5 = no tests collected
/// A missing code (killed by signal) is treated as an error.
fn parse_status(exit_code: Option<i32>, all_skipped: bool) -> TestStatus {
    match exit_code {
        Some(0) => {
            if all_skipped {
                TestStatus::Skipped
            } else {
                TestStatus::Passed
            }
        }
        Some(1) => TestStatus::Failed,
        Some(_) => TestStatus::Error,
        None => TestStatus::Error,
    }
}

/// Use `coverage json` to extract which files were covered
fn extract_covered_files(python_bin: &str, cov_file: &Path) -> Result<Vec<String>> {
    let json_file = cov_file.with_extension("json");

    let output = Command::new(python_bin)
        .args([
            "-m",
            "coverage",
            "json",
            "--data-file",
            cov_file.to_str().unwrap_or(".coverage"),
            "-o",
            json_file.to_str().unwrap_or("coverage.json"),
            "-q",
        ])
        .output()?;

    if !output.status.success() {
        return Ok(Vec::new());
    }

    let json_str = std::fs::read_to_string(&json_file)?;
    let v: serde_json::Value = serde_json::from_str(&json_str)?;

    let files: Vec<String> = v["files"]
        .as_object()
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default();

    // Clean up temp files
    let _ = std::fs::remove_file(&json_file);

    Ok(files)
}

fn print_progress(n: usize, total: usize, result: &TestResult) {
    let icon = match result.status {
        TestStatus::Passed => "✓".green().to_string(),
        TestStatus::Failed => "✗".red().to_string(),
        TestStatus::Error => "E".yellow().to_string(),
        TestStatus::Skipped => "s".dimmed().to_string(),
    };
    let duration = format!("{}ms", result.duration_ms).dimmed();
    println!(
        "  {} [{}/{}] {} {}",
        icon,
        n,
        total,
        result.test_id.dimmed(),
        duration
    );
}

/// Merge per-test coverage .coverage files into one via coverage combine
pub fn merge_coverage(
    python_bin: &str,
    coverage_dir: &Path,
) -> Result<HashMap<String, CoverageInfo>> {
    // Combine all .coverage.* files (its own output is not needed).
    Command::new(python_bin)
        .args([
            "-m",
            "coverage",
            "combine",
            "--keep",
            coverage_dir.to_str().unwrap_or("."),
        ])
        .output()?;

    // Generate JSON report
    let json_output = Command::new(python_bin)
        .args([
            "-m",
            "coverage",
            "json",
            "-o",
            ".riptide-coverage/combined.json",
            "-q",
        ])
        .output()?;

    if !json_output.status.success() {
        return Ok(HashMap::new());
    }

    let json_str = std::fs::read_to_string(".riptide-coverage/combined.json")?;
    let v: serde_json::Value = serde_json::from_str(&json_str)?;

    let mut coverage_map = HashMap::new();

    if let Some(files) = v["files"].as_object() {
        for (file, data) in files {
            let executed: Vec<u32> = data["executed_lines"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|n| n.as_u64().map(|x| x as u32))
                        .collect()
                })
                .unwrap_or_default();
            let missing: Vec<u32> = data["missing_lines"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|n| n.as_u64().map(|x| x as u32))
                        .collect()
                })
                .unwrap_or_default();
            let total = (executed.len() + missing.len()) as u32;
            let pct = if total > 0 {
                (executed.len() as f64 / total as f64) * 100.0
            } else {
                100.0
            };
            coverage_map.insert(
                file.clone(),
                CoverageInfo {
                    executed_lines: executed.len() as u32,
                    total_lines: total,
                    percentage: pct,
                },
            );
        }
    }

    Ok(coverage_map)
}

#[derive(Debug)]
pub struct CoverageInfo {
    pub executed_lines: u32,
    pub total_lines: u32,
    pub percentage: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_from_exit_codes() {
        assert_eq!(parse_status(Some(0), false), TestStatus::Passed);
        assert_eq!(parse_status(Some(0), true), TestStatus::Skipped);
        assert_eq!(parse_status(Some(1), false), TestStatus::Failed);
        // 5 = no tests collected (e.g. a bad node id) — an error, not a pass.
        assert_eq!(parse_status(Some(5), false), TestStatus::Error);
        assert_eq!(parse_status(Some(2), false), TestStatus::Error);
        // Killed by signal (timeout path passes None).
        assert_eq!(parse_status(None, false), TestStatus::Error);
    }

    #[test]
    fn batch_summary_parsing() {
        let out = "\
==== short test summary info ====
PASSED tests/test_mod_0.py::test_compute_0_0
PASSED tests/test_unit_case.py::ArithmeticCase::test_scale_0
SKIPPED tests/test_x.py::test_skipme
FAILED tests/test_mod_0.py::test_fail_demo - assert False
ERROR tests/test_y.py::test_broken
1 failed, 2 passed in 0.10s
";
        let m = parse_batch_summary(out);
        assert_eq!(
            m.get("tests/test_mod_0.py::test_compute_0_0"),
            Some(&TestStatus::Passed)
        );
        assert_eq!(
            m.get("tests/test_unit_case.py::ArithmeticCase::test_scale_0"),
            Some(&TestStatus::Passed)
        );
        assert_eq!(
            m.get("tests/test_x.py::test_skipme"),
            Some(&TestStatus::Skipped)
        );
        // The reason after ` - ` must not corrupt the node id key.
        assert_eq!(
            m.get("tests/test_mod_0.py::test_fail_demo"),
            Some(&TestStatus::Failed)
        );
        assert_eq!(
            m.get("tests/test_y.py::test_broken"),
            Some(&TestStatus::Error)
        );
        // The summary count line is not an outcome line.
        assert_eq!(m.len(), 5);
    }

    #[test]
    fn skip_detection() {
        assert!(looks_skipped("1 skipped in 0.01s"));
        assert!(!looks_skipped("1 passed in 0.01s"));
        assert!(!looks_skipped("1 failed, 1 skipped in 0.01s"));
        assert!(!looks_skipped("2 passed in 0.02s"));
    }

    #[test]
    fn short_hash_is_stable_and_distinct() {
        let a = short_hash("tests/a.py::test_x");
        let b = short_hash("tests/a.py::test_y");
        assert_eq!(a, short_hash("tests/a.py::test_x"));
        assert_ne!(a, b);
        assert_eq!(a.len(), 32); // 16 bytes hex-encoded
    }

    #[test]
    fn capped_read_truncates_oversized_output() {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        let big = vec![b'a'; MAX_CAPTURE_BYTES + 1024];
        f.write_all(&big).unwrap();
        let out = read_capped(f.path());
        assert!(out.len() < big.len());
        assert!(out.ends_with("[riptide] output truncated\n"));
    }
}
