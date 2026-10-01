//! `tiderace` — thin CLI front-end over `engine-core` (the engine owns the logic).
//!
//! - `tiderace collect <path>`: discover tests and print their node ids + styles.
//! - `tiderace run [options] <path>`: collect, execute, print a report, and set the pytest-style
//!   exit code. Needs `TIDERACE_SHIM` (path to `shim.py`); `TIDERACE_PYTHON` defaults to `python3`
//!   (`python` on Windows — see `engine_core::default_python`).
//!
//! `run` used to take no flags at all, so it always used one tier and one scheduler while the engine
//! shipped three and two (TID-17). Every measurement taken through it described that one combination
//! and got reported as "tiderace's performance", so the flags and the run header that names the
//! chosen configuration are equally the point.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use engine_core::collection::{Collector, RegexCollector};
use engine_core::domain::{Outcome, RunReport};
use engine_core::reporter::{JsonReporter, Reporter};
use engine_core::runner::{
    record_durations, run_parallel, RunPlan, SchedulerKind, VerdictStore, WorkerStrategy,
};

const USAGE: &str = "\
usage: tiderace <command> [options] <path>

Commands:
  collect <path>          discover tests and print their node ids + styles
  run [options] <path>    collect and execute, then report — through the daemon serving
                          <path> when one is (see `daemon`), else in this process
  daemon start <path>     keep a daemon for <path> warm: later runs import nothing
  daemon status <path>    is one serving, and is its image warm
  daemon stop <path>      shut it down

Options for `run`:
  -n, --workers <N>       parallel workers (default: the CPU count, capped by what memory allows
                          once the imported image's size is known; a count given here is used
                          as given)
      --memory-limit <MB> what the workers may occupy together, in megabytes: caps the pool
                          whatever the count. Also TIDERACE_MEMORY_LIMIT_MB, which the daemon
                          honours too
      --strategy <TIER>   isolation tier: fork | subinterp | subprocess
                          (default: fork on Unix, subprocess elsewhere)
      --scheduler <KIND>  batch packing: locality | round-robin (default: locality)
      --timeout <MS>      per-test deadline in milliseconds (default: 60000)
      --no-fork           alias for --strategy subprocess
      --optimistic        let restorable tests skip the fork (the default; kept for scripts)
      --no-optimistic     fork every test, even the restorable ones (see the note below)
      --shared-import     import the project once and fork the workers from it (the default)
      --shard-modules     split a module heavier than one worker's share across workers; a file
                          whose tests build on each other's state may then break (off by default)
      --no-shared-import  give every worker its own interpreter, each importing the project
  -m, --markers <EXPR>    run only tests matching a marker expression, e.g. 'not slow and db'.
                          Matches pytest marks and tiderace tags alike, and overrides any -m the
                          project sets in its own addopts
  -k, --keyword <EXPR>    run only tests whose name matches, e.g. 'TestClient and not slow'.
                          Case-insensitive substrings of the directories below the rootdir,
                          the file, class, function, case id and mark names, as pytest's -k
                          (so -k unit runs tests/unit/); overrides any -k in the project's
                          addopts
      --report <PATH>     also write a machine-readable JSON run report to PATH: one record per
                          node with its id, outcome, duration and flags. Compare runs by node id;
                          tallies hide two errors that cancel
      --strict-markers    error on a mark nothing declared — in the config, by a plugin, or
                          with tiderace.mark.register() in a conftest
  -q, --quiet             suppress the per-test lines; print only the tally
  -h, --help              show this message

Environment:
  TIDERACE_SHIM           path to shim.py (required if no bundled shim is installed)
  TIDERACE_PYTHON         interpreter to drive (default: python3 / python)
  TIDERACE_FORCE_FORK=1   same as --no-optimistic (the daemon already honours this)
  TIDERACE_MEMORY_LIMIT_MB what the workers may occupy together, as --memory-limit; the one the
                          daemon reads, since a run through it carries no flags
  TIDERACE_NO_SHARED_IMPORT=1  same as --no-shared-import

Notes:
  Restorable tests run in-process instead of forking, which is where most of the speed comes from:
  a fork copies the parent's page tables, so on a large-import project it costs far more than the
  test does. Anything the in-process path cannot restore is detected per test, re-run in a fork so
  this run still reports the right answer, and remembered so later runs fork it from the start.

  `--no-optimistic` forks every test. The one thing the detector cannot undo is a thread a test
  leaves running: it is caught and that test is demoted, but a neighbour counting threads still
  sees it. A forked child is a whole pristine process and has no such hole, so this is the setting
  to reach for when a suite disagrees with itself between the two.

  One Python parent imports the project once and forks a worker from it per core, rather than N
  parents each importing it. On a large-import project that halves CPU — an 8-worker run stops
  paying for eight imports — and takes ~20% off wall clock too, because simultaneous imports occupy
  the cores the tests want. `--no-shared-import` gives every worker its own interpreter again.
  Fork tier only, so it does nothing on Windows.

  `--strategy subinterp` is a hybrid: a sub-interpreter cannot load a single-phase C extension
  (numpy is the canonical case), so modules are probed and only the safe subset runs on the pool;
  the rest falls back. The run header says so.
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{USAGE}");
        return if args.is_empty() {
            ExitCode::from(64)
        } else {
            ExitCode::SUCCESS
        };
    }

    match args[0].as_str() {
        "collect" => match positional(&args[1..]) {
            Ok(root) => cmd_collect(&root),
            Err(e) => usage_error(&e),
        },
        "run" => match Options::parse(&args[1..]) {
            Ok(opts) => {
                // Handed to the shim through the environment: it is the process that reads the
                // project's own `addopts`, and it applies the same precedence pytest does — an
                // expression on the command line wins over one in the config (TID-59).
                if let Some(expr) = &opts.marker_expr {
                    // SAFETY: single-threaded here; workers are spawned further down.
                    unsafe { std::env::set_var("TIDERACE_MARKER_EXPR", expr) };
                }
                if let Some(expr) = &opts.keyword_expr {
                    // SAFETY: as above.
                    unsafe { std::env::set_var("TIDERACE_KEYWORD_EXPR", expr) };
                }
                if opts.strict_markers {
                    // The same route `-m` takes (TID-67): a project with no config file has
                    // nowhere else to say it, and the shim is what enforces it.
                    // SAFETY: as above.
                    unsafe { std::env::set_var("TIDERACE_STRICT_MARKERS", "1") };
                }
                // A run under `TIDERACE_NO_DAEMON` stays in this process — a gate that must not
                // share an image with earlier runs. A filtered run goes through the daemon with
                // its selection (TID-90): the image's workers apply it after the fork.
                let daemon = std::env::var_os("TIDERACE_NO_DAEMON").is_none().then(|| {
                    engine_core::exec::Selection {
                        keyword: opts.keyword_expr.clone(),
                        marker: opts.marker_expr.clone(),
                        strict_markers: opts.strict_markers,
                    }
                });
                cmd_run(
                    &opts.root,
                    &opts.plan,
                    opts.quiet,
                    opts.report.as_deref(),
                    daemon,
                )
            }
            Err(e) => usage_error(&e),
        },
        "daemon" => cmd_daemon(&args[1..]),
        other => usage_error(&format!("unknown command: {other}")),
    }
}

// ------------------------------------------------------------------ the daemon (TID-84)

/// One request to the daemon serving `root`, if one is listening: `None` when there is no socket
/// or nothing answers on it.
#[cfg(unix)]
fn daemon_call(
    root: &Path,
    request: engine_daemon::RpcRequest,
) -> Option<engine_daemon::RpcResponse> {
    use std::os::unix::net::UnixStream;
    let path = engine_daemon::daemon_socket_path(root);
    let mut stream = UnixStream::connect(&path).ok()?;
    engine_daemon::write_frame(&mut stream, &request).ok()?;
    engine_daemon::read_frame::<_, engine_daemon::RpcResponse>(&mut stream)
        .ok()
        .flatten()
}

#[cfg(not(unix))]
fn daemon_call(
    _root: &Path,
    _request: engine_daemon::RpcRequest,
) -> Option<engine_daemon::RpcResponse> {
    None
}

/// The full run through the daemon: `None` when none is serving this root, `Some(Err)` when one is
/// and could not run it, `Some(Ok(results))` otherwise — what the run here would have produced.
fn daemon_run(
    root: &Path,
    selection: engine_core::exec::Selection,
) -> Option<Result<Vec<engine_core::domain::TestResult>, String>> {
    use engine_daemon::{RpcRequest, RpcResponse};
    let root = root.canonicalize().ok()?;
    daemon_call(&root, RpcRequest::Health)?;
    let request = RpcRequest::RunFull {
        keyword: selection.keyword,
        marker: selection.marker,
        strict_markers: selection.strict_markers,
    };
    Some(match daemon_call(&root, request) {
        Some(RpcResponse::RanFull { results }) => Ok(results),
        Some(RpcResponse::Error { message }) => Err(message),
        Some(other) => Err(format!("unexpected answer: {other:?}")),
        None => Err("the daemon went away mid-run".to_string()),
    })
}

/// `tiderace daemon start|status|stop <path>`.
fn cmd_daemon(args: &[String]) -> ExitCode {
    use engine_daemon::{RpcRequest, RpcResponse};
    let (verb, root) = match args {
        [verb, root] => (verb.as_str(), PathBuf::from(root)),
        _ => return usage_error("daemon takes a verb and a path: daemon start|status|stop <path>"),
    };
    let root = root.canonicalize().unwrap_or(root);
    match verb {
        "status" => match daemon_call(&root, RpcRequest::Health) {
            Some(RpcResponse::Healthy { pid, warm }) => {
                println!(
                    "daemon serving {} — pid {pid}, image {}",
                    root.display(),
                    if warm {
                        "warm"
                    } else {
                        "cold (the first run imports the suite)"
                    }
                );
                ExitCode::SUCCESS
            }
            _ => {
                println!("no daemon serving {}", root.display());
                ExitCode::FAILURE
            }
        },
        "stop" => match daemon_call(&root, RpcRequest::Shutdown) {
            Some(RpcResponse::ShuttingDown) => {
                println!("daemon for {} stopped", root.display());
                ExitCode::SUCCESS
            }
            _ => {
                println!("no daemon serving {}", root.display());
                ExitCode::FAILURE
            }
        },
        "start" => daemon_start(&root),
        other => usage_error(&format!("unknown daemon verb: {other}")),
    }
}

#[cfg(unix)]
fn daemon_start(root: &Path) -> ExitCode {
    use engine_daemon::{RpcRequest, RpcResponse};
    use std::os::unix::process::CommandExt;
    if let Some(RpcResponse::Healthy { pid, .. }) = daemon_call(root, RpcRequest::Health) {
        println!("a daemon is already serving {} (pid {pid})", root.display());
        return ExitCode::SUCCESS;
    }
    // The daemon binary ships beside this one; `TIDERACE_DAEMON_BIN` points elsewhere.
    let bin = std::env::var("TIDERACE_DAEMON_BIN")
        .map(PathBuf::from)
        .or_else(|_| {
            std::env::current_exe().map(|exe| {
                exe.with_file_name(format!("tiderace-daemon{}", std::env::consts::EXE_SUFFIX))
            })
        })
        .unwrap_or_else(|_| PathBuf::from("tiderace-daemon"));
    let cache = root.join(".tiderace-cache");
    let log_path = cache.join("daemon.log");
    let log = std::fs::create_dir_all(&cache).and_then(|_| {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
    });
    let (out, err) = match log.and_then(|f| Ok((f.try_clone()?, f))) {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("error: cannot open {}: {e}", log_path.display());
            return ExitCode::FAILURE;
        }
    };
    let child = std::process::Command::new(&bin)
        .arg("serve")
        .arg(root)
        .stdin(std::process::Stdio::null())
        .stdout(out)
        .stderr(err)
        .process_group(0) // its own group: a Ctrl-C in this terminal does not take it down
        .spawn();
    let child = match child {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: cannot start {}: {e}", bin.display());
            return ExitCode::FAILURE;
        }
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while std::time::Instant::now() < deadline {
        if let Some(RpcResponse::Healthy { .. }) = daemon_call(root, RpcRequest::Health) {
            println!(
                "daemon started for {} (pid {}): the first run imports the suite, later runs do \
                 not. Log: {}",
                root.display(),
                child.id(),
                log_path.display()
            );
            return ExitCode::SUCCESS;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    eprintln!(
        "error: the daemon did not answer on {} within 30s — see {}",
        engine_daemon::daemon_socket_path(root).display(),
        log_path.display()
    );
    ExitCode::FAILURE
}

#[cfg(not(unix))]
fn daemon_start(_root: &Path) -> ExitCode {
    eprintln!("error: the daemon serves over a Unix socket, which this platform does not have");
    ExitCode::FAILURE
}

fn usage_error(msg: &str) -> ExitCode {
    eprintln!("error: {msg}\n\n{USAGE}");
    ExitCode::from(64)
}

/// The single positional path argument, for commands that take no flags.
fn positional(args: &[String]) -> Result<PathBuf, String> {
    match args {
        [p] if !p.starts_with('-') => Ok(PathBuf::from(p)),
        [] => Err("missing <path>".into()),
        _ => Err("expected exactly one <path>".into()),
    }
}

/// Parsed `run` invocation.
#[derive(Debug)]
struct Options {
    root: PathBuf,
    plan: RunPlan,
    quiet: bool,
    /// `-m EXPR`: the marker expression for this run, overriding the project's own `addopts`.
    marker_expr: Option<String>,
    /// `-k EXPR`: the name expression for this run, same precedence (TID-63).
    keyword_expr: Option<String>,
    /// `--strict-markers`: an undeclared mark is an error (TID-67).
    strict_markers: bool,
    /// `--report PATH`: where to write the per-node JSON report, if asked for.
    report: Option<PathBuf>,
}

impl Options {
    /// Hand-rolled because the whole binary has no dependencies beyond `engine-core`, and adding an
    /// argument-parsing crate for six flags is not a trade this CLI needs to make yet.
    ///
    /// Unknown flags and unparseable values are hard errors. Silently ignoring `--strategy subintrep`
    /// would run a different tier than asked for and report the result as though nothing happened —
    /// the precise failure mode TID-17 exists to end.
    fn parse(args: &[String]) -> Result<Self, String> {
        let mut plan = RunPlan::default();
        // The daemon has honoured `TIDERACE_FORCE_FORK=1` since before the ladder was a default;
        // read it here too so one setting covers both front ends. Applied before the flags, so an
        // explicit `--optimistic` on the command line still wins over it.
        if std::env::var("TIDERACE_FORCE_FORK").as_deref() == Ok("1") {
            plan.optimistic_no_fork = false;
        }
        if std::env::var("TIDERACE_NO_SHARED_IMPORT").as_deref() == Ok("1") {
            plan.shared_import = false;
        }
        let mut quiet = false;
        let mut marker_expr: Option<String> = None;
        let mut keyword_expr: Option<String> = None;
        let mut strict_markers = false;
        let mut report: Option<PathBuf> = None;
        let mut root: Option<PathBuf> = None;
        let mut strategy_set = false;

        let mut i = 0;
        while i < args.len() {
            let arg = args[i].as_str();
            let mut value = |name: &str| -> Result<String, String> {
                // Accept both `--flag value` and `--flag=value`.
                if let Some((_, v)) = arg.split_once('=') {
                    return Ok(v.to_string());
                }
                i += 1;
                args.get(i)
                    .cloned()
                    .ok_or_else(|| format!("{name} requires a value"))
            };
            let key = arg.split_once('=').map_or(arg, |(k, _)| k);

            match key {
                "-n" | "--workers" => {
                    let raw = value("--workers")?;
                    let n: usize = raw
                        .parse()
                        .map_err(|_| format!("--workers expects a number, got {raw:?}"))?;
                    if n == 0 {
                        return Err("--workers must be at least 1".into());
                    }
                    plan.workers = n;
                    plan.workers_explicit = true;
                }
                "--memory-limit" => {
                    let raw = value("--memory-limit")?;
                    let mb: u64 = raw
                        .parse()
                        .map_err(|_| format!("--memory-limit expects megabytes, got {raw:?}"))?;
                    if mb == 0 {
                        return Err("--memory-limit must be at least 1 MB".into());
                    }
                    plan.memory_limit_mb = Some(mb);
                }
                "--strategy" => {
                    let raw = value("--strategy")?;
                    plan.strategy = WorkerStrategy::parse(&raw).ok_or_else(|| {
                        format!(
                            "unknown --strategy {raw:?} (expected one of: {})",
                            WorkerStrategy::NAMES.join(", ")
                        )
                    })?;
                    strategy_set = true;
                }
                "--scheduler" => {
                    let raw = value("--scheduler")?;
                    plan.scheduler = SchedulerKind::parse(&raw).ok_or_else(|| {
                        format!(
                            "unknown --scheduler {raw:?} (expected one of: {})",
                            SchedulerKind::NAMES.join(", ")
                        )
                    })?;
                }
                "--timeout" => {
                    let raw = value("--timeout")?;
                    plan.deadline_ms = raw
                        .parse()
                        .map_err(|_| format!("--timeout expects milliseconds, got {raw:?}"))?;
                }
                "--no-fork" => {
                    plan.strategy = WorkerStrategy::Subprocess;
                    strategy_set = true;
                }
                // Accepted and inert: it is the default now, and it is in people's scripts.
                "--optimistic" => plan.optimistic_no_fork = true,
                "--no-optimistic" => plan.optimistic_no_fork = false,
                "--shared-import" => plan.shared_import = true,
                "--shard-modules" => plan.shard_modules = true,
                "--no-shared-import" => plan.shared_import = false,
                "-m" | "--markers" => marker_expr = Some(value("--markers")?),
                "-k" | "--keyword" => keyword_expr = Some(value("--keyword")?),
                "--strict-markers" => strict_markers = true,
                "--report" => report = Some(PathBuf::from(value("--report")?)),
                "-q" | "--quiet" => quiet = true,
                other if other.starts_with('-') => return Err(format!("unknown option: {other}")),
                _ => {
                    if root.replace(PathBuf::from(arg)).is_some() {
                        return Err("expected exactly one <path>".into());
                    }
                }
            }
            i += 1;
        }

        // Refuse an impossible tier at parse time rather than letting every batch fail inside a
        // worker thread, where it would read as an execution error rather than a bad request.
        if strategy_set && !plan.strategy.is_available() {
            return Err(format!(
                "--strategy {} is not available on this platform",
                plan.strategy
            ));
        }

        Ok(Self {
            root: root.ok_or("missing <path>")?,
            plan,
            quiet,
            marker_expr,
            keyword_expr,
            strict_markers,
            report,
        })
    }
}

fn cmd_collect(root: &Path) -> ExitCode {
    match RegexCollector::new().collect(root) {
        Ok(items) => {
            for item in &items {
                println!("{}\t{:?}", item.node_id, item.style);
            }
            eprintln!("collected {} tests", items.len());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// The plan a run will actually execute, plus the suffix describing what it learned from disk.
///
/// Split out because the header and the run must describe the same object, and they did not: the
/// header was built from a clamped copy while `run_parallel` received the unclamped original, so the
/// worker count shown was never the worker count used. Returning both from one place makes the two
/// impossible to disagree, and makes the whole decision unit-testable without a live run.
fn effective_plan(plan: &RunPlan, item_count: usize, root: &Path) -> (RunPlan, String) {
    // **`must_fork` only.** The two persisted verdicts fail in opposite directions, and only one is
    // safe to take from a file this process did not write and cannot re-verify:
    //
    //   * `must_fork` only ever *removes* an optimisation. Acting on a stale one forks a test that
    //     no longer needs it — a little time, never a wrong answer.
    //   * `trusted_pure` promotes a test to the bare no-fork tier, which skips the snapshot
    //     entirely. `VerdictStore::trusted_pure` guards it by re-hashing every recorded dependency,
    //     but that guard is only as good as the footprints, and they are unsound today (TID-40): a
    //     module's imports execute once, for whichever test runs first, so on a 20-tests-per-module
    //     suite the source under test appears in one footprint out of twenty. The other nineteen
    //     would keep a stale `pure` verdict through a change to the very code they exercise.
    //
    // So `trusted_pure` stays unwired until TID-40 lands. Waiting costs nothing measurable: on the
    // reference fixture, reading it changed wall clock by 0.01s.
    let store = VerdictStore::load(root);
    let must_fork = store.must_fork();
    // Durations too (TID-62). Safe from a file nobody re-verified for the same reason `must_fork`
    // is: they only order work, so a stale one costs a little balance and never a wrong answer.
    let durations = store.durations();
    let mut learned: Vec<String> = Vec::new();
    if !must_fork.is_empty() {
        learned.push(format!("{} forced-fork", must_fork.len()));
    }
    if !durations.is_empty() {
        learned.push(format!("{} durations", durations.len()));
    }
    let learned = if learned.is_empty() {
        String::new()
    } else {
        format!(" learned={}", learned.join(","))
    };
    let effective = RunPlan {
        workers: plan.effective_workers(item_count),
        must_fork,
        durations,
        ..plan.clone()
    };
    (effective, learned)
}

fn cmd_run(
    root: &Path,
    plan: &RunPlan,
    quiet: bool,
    report_path: Option<&Path>,
    daemon: Option<engine_core::exec::Selection>,
) -> ExitCode {
    let mut timing = engine_core::runner::PhaseTimer::start("tiderace", "cli");
    let engine_core::Target { python, shim } = match engine_core::resolve_target() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    // A daemon serving this root runs it from its warm image (TID-84): the same results, reported
    // here the same way, and the daemon persists durations and verdicts itself. Asked first, so a
    // run it serves never walks the tree here (TID-94). No daemon, or one that refuses, and the
    // run happens in this process as before.
    timing.mark("before daemon");
    let via_daemon = daemon.and_then(|selection| daemon_run(root, selection));
    timing.mark("daemon round trip");
    let results = match via_daemon {
        Some(Ok(results)) => {
            let (effective, learned) = effective_plan(plan, results.len(), root);
            eprintln!("tiderace: {}{learned} via daemon", effective.header());
            results
        }
        Some(Err(message)) => {
            eprintln!("error: the daemon could not run this: {message}");
            return ExitCode::FAILURE;
        }
        None => {
            let items = match RegexCollector::new().collect(root) {
                Ok(items) => items,
                Err(e) => {
                    eprintln!("error: collection failed: {e}");
                    return ExitCode::FAILURE;
                }
            };

            // Pick up what earlier runs learned. The daemon writes these verdicts; `run` reads
            // them and writes nothing, so a one-shot command never mutates the tree and never
            // needs coverage capture turned on — the dependency footprints that keep a purity
            // verdict honest are already recorded, and checking them is a re-hash.
            let (effective, learned) = effective_plan(plan, items.len(), root);
            eprintln!("tiderace: {}{learned}", effective.header());

            // `&effective`, not `plan`: the header and the run must describe the same thing. They
            // did not, so the worker clamp shown in the header was never the clamp applied — and
            // the verdicts read above would have been reported and then dropped on the floor.
            let results = match run_parallel(&python, &shim, root, items, &effective) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("error: {e}");
                    return ExitCode::FAILURE;
                }
            };

            // The one thing `run` writes back (TID-62, ADR-E016): how long each node took, so the
            // next run's scheduler hands out the heaviest module first instead of the one with the
            // most tests. Not a verdict — the verdict store's "reading only" contract still holds
            // for everything that can change an answer — and best-effort: a tree that cannot be
            // written runs cold next time, which is not a failure of this run.
            if let Err(e) = record_durations(root, &results) {
                eprintln!(
                    "warning: could not record durations in {}: {e}",
                    root.display()
                );
            }
            results
        }
    };
    let report = RunReport::new(results);
    if !quiet {
        for result in &report.results {
            println!("{}\t{}", label(result.outcome), result.node_id);
            // The shim computes a `detail` for every non-pass outcome; printing only the label
            // discarded it, leaving a failing run with no way to say what broke. Indented under its
            // node so the pass/fail column stays scannable.
            if !matches!(result.outcome, Outcome::Passed | Outcome::Skipped)
                && !result.detail.is_empty()
            {
                for line in result.detail.lines() {
                    println!("    {line}");
                }
            }
        }
    }
    // A skip count answers two different questions and pytest's summary answers only one of them.
    // `pytest.importorskip` in a module skips every test it holds: pytest prints one skip, we print
    // hundreds, and side by side the two look like a disagreement — during the benchmark that gap
    // cost hours of chasing a defect that was not there (TID-55). So print both numbers.
    let modules = report.skipped_modules();
    let at_import = if modules == 0 {
        String::new()
    } else {
        format!(
            " ({modules} module{} skipped at import)",
            if modules == 1 { "" } else { "s" }
        )
    };
    eprintln!(
        "{} passed, {} failed, {} error, {} skipped{at_import}, {} total",
        report.tally(Outcome::Passed),
        report.tally(Outcome::Failed),
        report.tally(Outcome::Error),
        report.tally(Outcome::Skipped),
        report.total(),
    );
    // Each worker's peak resident size, where the platform reports it (TID-106): the number to
    // know before running a suite on a smaller box, and the one `--memory-limit` acts on.
    {
        let mut peaks: std::collections::BTreeMap<usize, u64> = Default::default();
        for r in &report.results {
            if let (Some(w), Some(mb)) = (r.worker, r.worker_peak_rss_mb) {
                let e = peaks.entry(w).or_insert(0);
                *e = (*e).max(mb);
            }
        }
        if !peaks.is_empty() {
            let total: u64 = peaks.values().sum();
            let max = peaks.values().copied().max().unwrap_or(0);
            eprintln!(
                "tiderace: memory: {} worker{}, peak {max} MB each at most, {total} MB together",
                peaks.len(),
                if peaks.len() == 1 { "" } else { "s" }
            );
        }
    }
    if let Some(path) = report_path {
        // Written after the summary so a failure to write is the last thing on the terminal, and
        // non-fatal: the run's own verdict is what the exit code is for, and losing the report file
        // must not turn a green suite red.
        if let Err(e) = std::fs::write(path, JsonReporter.render(&report)) {
            eprintln!("warning: could not write --report {}: {e}", path.display());
        }
    }
    ExitCode::from(report.exit_code() as u8)
}

fn label(outcome: Outcome) -> &'static str {
    match outcome {
        Outcome::Passed => "PASS",
        Outcome::Failed => "FAIL",
        Outcome::Error => "ERROR",
        Outcome::Skipped => "SKIP",
        Outcome::XFail => "XFAIL",
        Outcome::XPass => "XPASS",
    }
}

#[cfg(test)]
mod tests {
    use super::{effective_plan, Options};
    use engine_core::domain::Outcome;
    use engine_core::runner::RecordedOutcome;
    use engine_core::runner::{RunPlan, SchedulerKind, WorkerStrategy, DEFAULT_DEADLINE_MS};

    fn parse(args: &[&str]) -> Result<Options, String> {
        Options::parse(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn bare_path_uses_platform_defaults() {
        let o = parse(&["tests"]).expect("a bare path is a valid run");
        assert_eq!(o.root.to_str(), Some("tests"));
        assert_eq!(o.plan.strategy, WorkerStrategy::platform_default());
        assert_eq!(o.plan.scheduler, SchedulerKind::Locality);
        assert_eq!(o.plan.deadline_ms, DEFAULT_DEADLINE_MS);
        assert!(!o.quiet);
    }

    #[test]
    fn the_keyword_expression_parses_in_both_spellings() {
        for args in [
            vec!["-k", "TestClient and not slow", "tests"],
            vec!["--keyword", "TestClient and not slow", "tests"],
            vec!["--keyword=TestClient and not slow", "tests"],
        ] {
            let o = parse(&args).expect("-k parses");
            assert_eq!(o.keyword_expr.as_deref(), Some("TestClient and not slow"));
        }
        assert!(parse(&["tests"]).unwrap().keyword_expr.is_none());
    }

    #[test]
    fn strict_markers_is_a_bare_flag_off_by_default() {
        assert!(!parse(&["tests"]).unwrap().strict_markers);
        assert!(
            parse(&["--strict-markers", "tests"])
                .unwrap()
                .strict_markers
        );
    }

    #[test]
    fn the_report_path_parses_in_both_spellings_and_is_off_by_default() {
        assert!(
            parse(&["tests"])
                .expect("a bare path is a valid run")
                .report
                .is_none(),
            "no file is written unless one is asked for"
        );
        for args in [
            vec!["--report", "/tmp/run.json", "tests"],
            vec!["--report=/tmp/run.json", "tests"],
        ] {
            let o = parse(&args).expect("--report parses");
            assert_eq!(
                o.report.as_deref(),
                Some(std::path::Path::new("/tmp/run.json"))
            );
        }
        assert!(
            parse(&["--report", "tests"]).is_err(),
            "--report without a path is a usage error, not a silently dropped flag"
        );
    }

    #[test]
    fn every_knob_parses_in_both_spellings() {
        for args in [
            vec![
                "--workers",
                "3",
                "--scheduler",
                "round-robin",
                "--timeout",
                "99",
                "tests",
            ],
            vec![
                "--workers=3",
                "--scheduler=round-robin",
                "--timeout=99",
                "tests",
            ],
        ] {
            let o = parse(&args).expect("flags parse");
            assert_eq!(o.plan.workers, 3);
            assert_eq!(o.plan.scheduler, SchedulerKind::RoundRobin);
            assert_eq!(o.plan.deadline_ms, 99);
            assert_eq!(o.root.to_str(), Some("tests"));
        }
    }

    #[test]
    fn flags_may_follow_the_path() {
        let o = parse(&["tests", "-n", "2"]).expect("order must not matter");
        assert_eq!(o.plan.workers, 2);
        assert_eq!(o.root.to_str(), Some("tests"));
    }

    #[test]
    fn no_fork_is_an_alias_for_the_subprocess_tier() {
        let o = parse(&["--no-fork", "tests"]).expect("--no-fork parses");
        assert_eq!(o.plan.strategy, WorkerStrategy::Subprocess);
    }

    #[test]
    fn quiet_and_optimistic_are_recorded() {
        let o = parse(&["-q", "--optimistic", "tests"]).expect("parses");
        assert!(o.quiet);
        assert!(o.plan.optimistic_no_fork);
        assert!(o.plan.header().contains("optimistic-no-fork"));
    }

    /// The header and the run describe the same plan.
    ///
    /// They used to not: the header was built from a clamped copy while `run_parallel` got the
    /// unclamped original, so a run that announced `workers=3` could execute with 16. The verdicts
    /// read from disk went the same way — reported in the header, then dropped.
    #[test]
    fn the_plan_that_is_reported_is_the_plan_that_runs() {
        let dir = std::env::temp_dir().join(format!("tiderace_cli_plan_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let asked = RunPlan {
            workers: 16,
            ..RunPlan::default()
        };
        let (effective, _) = effective_plan(&asked, 3, &dir);
        assert_eq!(
            effective.workers, 3,
            "a 3-test corpus clamps to 3 workers, and that is what must execute"
        );
        assert!(effective.header().contains("workers=3"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Recorded state-disturbers reach the plan; purity verdicts deliberately do not (TID-40).
    #[test]
    fn must_fork_is_read_from_disk_and_trusted_pure_is_not() {
        use engine_core::runner::{PersistedState, TestRecord, STATE_FILE};

        let dir = std::env::temp_dir().join(format!("tiderace_cli_verdict_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("src.py"),
            "X = 1
",
        )
        .unwrap();

        let mut state = PersistedState::default();
        state.files.insert(
            "src.py".into(),
            engine_core::runner::hash_file_or_missing(&dir, "src.py"),
        );
        state.tests.insert(
            "t.py::disturber".into(),
            TestRecord {
                outcome: RecordedOutcome::Ran(Outcome::Passed),
                detail: String::new(),
                deps: vec!["src.py".into()],
                pure: Some(false),
                must_fork: true,
                keywords: Vec::new(),
                skip_origin: None,
            },
        );
        state.tests.insert(
            "t.py::clean".into(),
            TestRecord {
                outcome: RecordedOutcome::Ran(Outcome::Passed),
                detail: String::new(),
                deps: vec!["src.py".into()],
                pure: Some(true),
                must_fork: false,
                keywords: Vec::new(),
                skip_origin: None,
            },
        );
        state.save(&dir.join(STATE_FILE)).unwrap();

        let (effective, learned) = effective_plan(&RunPlan::default(), 2, &dir);
        assert!(effective.must_fork.contains("t.py::disturber"));
        assert!(
            effective.trusted_pure.is_empty(),
            "purity verdicts stay unwired until TID-40 makes the footprints sound"
        );
        assert!(learned.contains("1 forced-fork"), "got: {learned:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn shared_import_is_the_default_and_the_header_says_which() {
        let d = parse(&["tests"]).expect("parses");
        assert!(d.plan.shared_import);
        assert!(d.plan.header().contains("shared-import"));

        let off = parse(&["--no-shared-import", "tests"]).expect("parses");
        assert!(!off.plan.shared_import);
        let header = off.plan.header();
        assert!(header.contains("import-per-worker"), "got: {header}");
        assert!(!header.contains(" shared-import"), "got: {header}");
    }

    /// Kept parsing and inert, because it is in scripts written while it was opt-in.
    #[test]
    fn the_old_shared_import_flag_still_parses() {
        assert!(
            parse(&["--shared-import", "tests"])
                .expect("the old flag still parses")
                .plan
                .shared_import
        );
    }

    #[test]
    fn the_last_shared_import_flag_wins() {
        assert!(
            !parse(&["--shared-import", "--no-shared-import", "tests"])
                .expect("parses")
                .plan
                .shared_import
        );
        assert!(
            parse(&["--no-shared-import", "--shared-import", "tests"])
                .expect("parses")
                .plan
                .shared_import
        );
    }

    #[test]
    fn the_optimistic_ladder_is_the_default() {
        assert!(parse(&["tests"]).expect("parses").plan.optimistic_no_fork);
    }

    #[test]
    fn no_optimistic_forks_every_test_and_the_header_says_so() {
        let o = parse(&["--no-optimistic", "tests"]).expect("parses");
        assert!(!o.plan.optimistic_no_fork);
        assert!(o.plan.header().contains("fork-per-test"));
    }

    /// `--optimistic` is inert now, but it is in people's scripts and CI configs, so it has to keep
    /// parsing rather than becoming `unknown option`.
    #[test]
    fn the_old_optimistic_flag_still_parses() {
        let o = parse(&["--optimistic", "tests"]).expect("the old flag still parses");
        assert!(o.plan.optimistic_no_fork);
    }

    /// Last flag wins, so a script that appends `--no-optimistic` to an existing `--optimistic`
    /// invocation gets what it asked for.
    #[test]
    fn the_last_of_the_two_flags_wins() {
        assert!(
            !parse(&["--optimistic", "--no-optimistic", "tests"])
                .expect("parses")
                .plan
                .optimistic_no_fork
        );
        assert!(
            parse(&["--no-optimistic", "--optimistic", "tests"])
                .expect("parses")
                .plan
                .optimistic_no_fork
        );
    }

    /// A typo must stop the run. Falling through to the default would execute a different tier than
    /// asked for and report it as a clean run.
    #[test]
    fn unknown_values_and_flags_are_hard_errors() {
        let err = parse(&["--strategy", "subintrep", "tests"]).expect_err("typo must fail");
        assert!(
            err.contains("subintrep"),
            "message must quote the input: {err}"
        );
        assert!(
            err.contains("fork"),
            "message must list the valid tiers: {err}"
        );

        assert!(parse(&["--scheduler", "locallity", "tests"]).is_err());
        assert!(parse(&["--nonsense", "tests"]).is_err());
        assert!(parse(&["--workers", "many", "tests"]).is_err());
        assert!(parse(&["--timeout", "soon", "tests"]).is_err());
    }

    #[test]
    fn missing_values_and_paths_are_reported() {
        assert!(parse(&["--workers"]).is_err());
        assert!(parse(&["--strategy"]).is_err());
        assert!(parse(&[]).is_err(), "a run needs a path");
        assert!(parse(&["a", "b"]).is_err(), "two paths are ambiguous");
    }

    #[test]
    fn zero_workers_is_refused_rather_than_silently_clamped() {
        assert!(parse(&["--workers", "0", "tests"]).is_err());
    }

    #[test]
    fn each_advertised_strategy_name_is_accepted_where_available() {
        for name in WorkerStrategy::NAMES {
            let parsed = WorkerStrategy::parse(name).expect("advertised name parses");
            let got = parse(&["--strategy", name, "tests"]);
            if parsed.is_available() {
                assert_eq!(got.expect("available tier parses").plan.strategy, parsed);
            } else {
                // Refused at parse time, with the tier named.
                assert!(got.is_err(), "{name} is unavailable and must be refused");
            }
        }
    }

    #[test]
    fn the_header_states_the_configuration() {
        let o = parse(&["--scheduler", "round-robin", "-n", "2", "tests"]).expect("parses");
        let h = o.plan.header();
        assert!(
            h.contains("round-robin") && h.contains("workers=2"),
            "got {h}"
        );
    }
}
