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
use engine_core::runner::{run_parallel, RunPlan, SchedulerKind, WorkerStrategy};

const USAGE: &str = "\
usage: tiderace <command> [options] <path>

Commands:
  collect <path>          discover tests and print their node ids + styles
  run [options] <path>    collect and execute, then report

Options for `run`:
  -n, --workers <N>       parallel workers (default: CPU count; 1 = sequential)
      --strategy <TIER>   isolation tier: fork | subinterp | subprocess
                          (default: fork on Unix, subprocess elsewhere)
      --scheduler <KIND>  batch packing: locality | round-robin (default: locality)
      --timeout <MS>      per-test deadline in milliseconds (default: 5000)
      --no-fork           alias for --strategy subprocess
      --optimistic        let restorable tests skip the fork (~2.4x; see the note below)
  -q, --quiet             suppress the per-test lines; print only the tally
  -h, --help              show this message

Environment:
  TIDERACE_SHIM           path to shim.py (required if no bundled shim is installed)
  TIDERACE_PYTHON         interpreter to drive (default: python3 / python)

Notes:
  `--optimistic` runs restorable tests in-process instead of forking (~2.4x on a large corpus), but
  snapshot/restore only covers the TEST module's globals. A test that mutates a library module's
  state — registering into a registry, installing a pack — leaks into the next test. Forking every
  test has no such hole, so it is the default.

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
            Ok(opts) => cmd_run(&opts.root, &opts.plan, opts.quiet),
            Err(e) => usage_error(&e),
        },
        other => usage_error(&format!("unknown command: {other}")),
    }
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
        let mut quiet = false;
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
                "--optimistic" => plan.optimistic_no_fork = true,
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

fn cmd_run(root: &Path, plan: &RunPlan, quiet: bool) -> ExitCode {
    let python = std::env::var("TIDERACE_PYTHON").unwrap_or_else(|_| engine_core::default_python());
    let shim = match std::env::var("TIDERACE_SHIM") {
        Ok(s) => PathBuf::from(s),
        Err(_) => match engine_core::default_shim(&python) {
            Some(p) => p, // shim shipped inside the installed `tiderace` package
            None => {
                eprintln!(
                    "error: TIDERACE_SHIM not set and no bundled shim found — \
                     `pip install tiderace` into this interpreter, or point TIDERACE_SHIM at py-shim/shim.py"
                );
                return ExitCode::FAILURE;
            }
        },
    };

    let items = match RegexCollector::new().collect(root) {
        Ok(items) => items,
        Err(e) => {
            eprintln!("error: collection failed: {e}");
            return ExitCode::FAILURE;
        }
    };

    // The header goes out BEFORE the run, so a run that later hangs or dies has still said what it
    // was doing — which is exactly when you most want to know.
    let effective = RunPlan {
        workers: plan.effective_workers(items.len()),
        ..plan.clone()
    };
    eprintln!("tiderace: {}", effective.header());

    let results = match run_parallel(&python, &shim, root, items, plan) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
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
    eprintln!(
        "{} passed, {} failed, {} error, {} skipped, {} total",
        report.tally(Outcome::Passed),
        report.tally(Outcome::Failed),
        report.tally(Outcome::Error),
        report.tally(Outcome::Skipped),
        report.total(),
    );
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
    use super::Options;
    use engine_core::runner::{SchedulerKind, WorkerStrategy, DEFAULT_DEADLINE_MS};

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

    #[test]
    fn forking_every_test_is_the_default() {
        assert!(!parse(&["tests"]).expect("parses").plan.optimistic_no_fork);
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
