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

        // Isolated: one pytest process per test. Only used when the caller asks
        // for it explicitly — coverage no longer forces isolation (see below).
        if self.isolate {
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

        // Batched: one pytest process per worker. With coverage, each batch runs
        // under `coverage run` with a per-test dynamic context, so we still get a
        // precise per-test dependency graph from a fast batched run (ADR-011).
        if self.with_coverage {
            let _ = self.write_coverage_rc();
        }
        let chunk_size = tests.len().div_ceil(self.workers.max(1)).max(1);
        let chunks: Vec<&[TestItem]> = tests.chunks(chunk_size).collect();
        let mut results: Vec<TestResult> = pool.install(|| {
            chunks
                .par_iter()
                .map(|chunk| self.run_chunk(chunk, total, &counter))
                .reduce(Vec::new, |mut acc, mut v| {
                    acc.append(&mut v);
                    acc
                })
        });

        // Attach per-test dependencies extracted from coverage contexts.
        if self.with_coverage {
            let deps = self.extract_context_deps(tests).unwrap_or_default();
            for r in &mut results {
                if let Some(files) = deps.get(&r.test_id) {
                    r.covered_files = files.clone();
                }
            }
        }
        Ok(results)
    }

    /// Write a coverage config enabling per-test dynamic contexts, so a single
    /// batched `coverage run` records which lines each test touched.
    fn write_coverage_rc(&self) -> Result<PathBuf> {
        let rc = self.coverage_dir.join(".riptide.coveragerc");
        std::fs::write(
            &rc,
            "[run]\ndynamic_context = test_function\nbranch = True\nsource = .\n",
        )?;
        Ok(rc)
    }

    /// Combine the per-batch coverage data and read `--show-contexts` JSON to build
    /// a `test_id -> covered files` map. Context names are dotted module paths
    /// (`tests.test_x.TestC.test_m`) that map deterministically back to node ids.
    fn extract_context_deps(&self, tests: &[TestItem]) -> Result<HashMap<String, Vec<String>>> {
        Command::new(&self.python_bin)
            .args(["-m", "coverage", "combine", "--keep"])
            .arg(&self.coverage_dir)
            .output()?;
        let json_path = self.coverage_dir.join("contexts.json");
        let out = Command::new(&self.python_bin)
            .args(["-m", "coverage", "json", "--show-contexts", "-q", "-o"])
            .arg(&json_path)
            .output()?;
        if !out.status.success() || !json_path.exists() {
            return Ok(HashMap::new());
        }
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&json_path)?)?;
        Ok(contexts_to_deps(&v, tests))
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
        // Per-batch coverage data file (kept for the combine/contexts pass).
        let cov_data = if self.with_coverage {
            Some(
                self.coverage_dir
                    .join(format!(".coverage.{}", short_hash(&node_ids.join("\n")))),
            )
        } else {
            None
        };

        let statuses = match self.exec_chunk(&node_ids, &out_path, cov_data.as_deref()) {
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
                // A parametrized test runs as many `nodeid[param]` cases, so we
                // aggregate every reported case under its base node id. Missing
                // from the summary => no outcome reported => Error.
                let status = aggregate_status(&node_id, &statuses).unwrap_or(TestStatus::Error);
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
        cov_data: Option<&Path>,
    ) -> Result<HashMap<String, TestStatus>> {
        let out_file = File::create(out_path)?;
        let mut cmd = Command::new(&self.python_bin);
        // Optionally wrap pytest in `coverage run` with per-test dynamic contexts.
        if let Some(data) = cov_data {
            let rc = self.coverage_dir.join(".riptide.coveragerc");
            cmd.arg("-m")
                .arg("coverage")
                .arg("run")
                .arg("--rcfile")
                .arg(&rc);
            cmd.arg("--data-file").arg(data).arg("-m").arg("pytest");
        } else {
            cmd.arg("-m").arg("pytest");
        }
        // -rA prints an outcome line per test; no -x so the whole batch runs.
        cmd.args(["-rA", "--tb=no", "-q", "-p", "no:cacheprovider", "--"]);
        cmd.args(node_ids);
        cmd.stdout(Stdio::from(out_file));
        cmd.stderr(Stdio::null());
        crate::procutil::set_process_group(&mut cmd);

        let mut child = cmd
            .spawn()
            .context("failed to spawn batched test process")?;
        match child.wait_timeout(self.timeout)? {
            Some(_) => {}
            None => {
                // Timed out: kill the whole process group, then report nothing so
                // every test in the batch becomes an Error.
                crate::procutil::kill_tree(&mut child);
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
        crate::procutil::set_process_group(&mut cmd);

        let mut child = cmd.spawn().context("failed to spawn test subprocess")?;

        let (exit_code, timed_out) = match child.wait_timeout(self.timeout)? {
            Some(exit) => (exit.code(), false),
            None => {
                // Exceeded the limit — kill the whole process group and reap it.
                crate::procutil::kill_tree(&mut child);
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

/// Map a `coverage json --show-contexts` document to `test_id -> covered files`.
/// Pure (no I/O) so the suffix-matching logic is directly unit-tested.
fn contexts_to_deps(v: &serde_json::Value, tests: &[TestItem]) -> HashMap<String, Vec<String>> {
    // context name -> files it executed lines in.
    let mut ctx_files: HashMap<String, Vec<String>> = HashMap::new();
    if let Some(files) = v["files"].as_object() {
        for (fname, fdata) in files {
            if let Some(contexts) = fdata["contexts"].as_object() {
                for ctxs in contexts.values() {
                    for c in ctxs.as_array().into_iter().flatten() {
                        if let Some(c) = c.as_str() {
                            if !c.is_empty() {
                                ctx_files
                                    .entry(c.to_string())
                                    .or_default()
                                    .push(fname.clone());
                            }
                        }
                    }
                }
            }
        }
    }
    // Index by the last 2 and 3 dotted components, so a test's stable suffix
    // (`stem.func` or `stem.Class.method`) matches regardless of the package
    // prefix coverage prepends.
    let mut by_tail: HashMap<String, Vec<String>> = HashMap::new();
    for (ctx, files) in &ctx_files {
        let parts: Vec<&str> = ctx.split('.').collect();
        for n in [2usize, 3] {
            if parts.len() >= n {
                let tail = parts[parts.len() - n..].join(".");
                by_tail
                    .entry(tail)
                    .or_default()
                    .extend(files.iter().cloned());
            }
        }
    }
    for files in by_tail.values_mut() {
        files.sort();
        files.dedup();
    }

    let mut deps = HashMap::new();
    for test in tests {
        if let Some(files) = by_tail.get(&expected_suffix(test)) {
            deps.insert(test.test_id.clone(), files.clone());
        }
    }
    deps
}

/// The trailing components of the coverage dynamic-context name for a test:
/// `{file_stem}.{func}` or `{file_stem}.{Class}.{method}`. Coverage prefixes the
/// context with the full dotted module path (which varies with package layout,
/// e.g. `pkg.tests.test_x.test_a`), so we match on this stable suffix instead of
/// trying to predict the whole name.
fn expected_suffix(item: &TestItem) -> String {
    let stem = Path::new(&item.file_path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(&item.file_path);
    match &item.class_name {
        Some(c) => format!("{}.{}.{}", stem, c, item.function_name),
        None => format!("{}.{}", stem, item.function_name),
    }
}

/// Aggregate the outcome of a collected test from the batch summary, folding in
/// every parametrized case. pytest reports a parametrized `tests/t.py::test_x` as
/// `test_x[1]`, `test_x[2]`, … — so we match the exact node id *and* any
/// `node_id[...]` case. Precedence: a failure wins, then an error, then a pass;
/// only if every case skipped is the test skipped. Returns `None` if nothing
/// matched (the test never reported an outcome).
fn aggregate_status(node_id: &str, statuses: &HashMap<String, TestStatus>) -> Option<TestStatus> {
    let param_prefix = format!("{}[", node_id);
    let mut any = false;
    let mut failed = false;
    let mut errored = false;
    let mut passed = false;
    for (key, status) in statuses {
        if key == node_id || key.starts_with(&param_prefix) {
            any = true;
            match status {
                TestStatus::Failed => failed = true,
                TestStatus::Error => errored = true,
                TestStatus::Passed => passed = true,
                TestStatus::Skipped => {}
            }
        }
    }
    if !any {
        return None;
    }
    Some(if failed {
        TestStatus::Failed
    } else if errored {
        TestStatus::Error
    } else if passed {
        TestStatus::Passed
    } else {
        TestStatus::Skipped
    })
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
    fn context_suffix_from_node_id() {
        let func = TestItem {
            test_id: "tests/test_add.py::test_add".into(),
            file_path: "tests/test_add.py".into(),
            function_name: "test_add".into(),
            class_name: None,
        };
        // Stem-based suffix; matches `pkg.tests.test_add.test_add` regardless of prefix.
        assert_eq!(expected_suffix(&func), "test_add.test_add");
        let method = TestItem {
            test_id: "tests/test_x.py::TestC::test_m".into(),
            file_path: "tests/test_x.py".into(),
            function_name: "test_m".into(),
            class_name: Some("TestC".into()),
        };
        assert_eq!(expected_suffix(&method), "test_x.TestC.test_m");
    }

    #[test]
    fn contexts_map_to_per_test_deps() {
        // Synthetic `coverage json --show-contexts` with a package prefix on the
        // context names (the case that broke naive full-name prediction).
        let json = serde_json::json!({
            "files": {
                "src/a.py": { "contexts": { "1": ["", "pkg.tests.test_a.test_a"] } },
                "tests/test_a.py": { "contexts": { "2": ["pkg.tests.test_a.test_a"] } },
                "src/b.py": { "contexts": { "1": ["pkg.tests.test_b.TestB.test_b"] } },
            }
        });
        let tests = vec![
            TestItem {
                test_id: "tests/test_a.py::test_a".into(),
                file_path: "tests/test_a.py".into(),
                function_name: "test_a".into(),
                class_name: None,
            },
            TestItem {
                test_id: "tests/test_b.py::TestB::test_b".into(),
                file_path: "tests/test_b.py".into(),
                function_name: "test_b".into(),
                class_name: Some("TestB".into()),
            },
        ];
        let deps = contexts_to_deps(&json, &tests);
        assert_eq!(
            deps.get("tests/test_a.py::test_a"),
            Some(&vec!["src/a.py".to_string(), "tests/test_a.py".to_string()])
        );
        assert_eq!(
            deps.get("tests/test_b.py::TestB::test_b"),
            Some(&vec!["src/b.py".to_string()])
        );
    }

    #[test]
    fn aggregates_parametrized_cases() {
        let mut s = HashMap::new();
        s.insert("t.py::test_p[1]".to_string(), TestStatus::Passed);
        s.insert("t.py::test_p[2]".to_string(), TestStatus::Passed);
        s.insert("t.py::test_p[3]".to_string(), TestStatus::Passed);
        // All params passed → the base test passes (was wrongly Error before).
        assert_eq!(
            aggregate_status("t.py::test_p", &s),
            Some(TestStatus::Passed)
        );

        s.insert("t.py::test_p[2]".to_string(), TestStatus::Failed);
        // Any failing param → the base test fails.
        assert_eq!(
            aggregate_status("t.py::test_p", &s),
            Some(TestStatus::Failed)
        );

        // A plain (non-param) test still matches exactly.
        let mut p = HashMap::new();
        p.insert("t.py::test_plain".to_string(), TestStatus::Passed);
        assert_eq!(
            aggregate_status("t.py::test_plain", &p),
            Some(TestStatus::Passed)
        );

        // Not reported at all → None (caller treats as Error).
        assert_eq!(aggregate_status("t.py::missing", &p), None);

        // All cases skipped → skipped.
        let mut sk = HashMap::new();
        sk.insert("t.py::test_s[a]".to_string(), TestStatus::Skipped);
        sk.insert("t.py::test_s[b]".to_string(), TestStatus::Skipped);
        assert_eq!(
            aggregate_status("t.py::test_s", &sk),
            Some(TestStatus::Skipped)
        );
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
