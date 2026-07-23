//! `tiderace` — thin CLI front-end over `engine-core` (the engine owns the logic). Phase 2 commands:
//!
//! - `tiderace collect <path>`: discover tests and print their node ids + styles.
//! - `tiderace run <path>`: collect, fork-execute via the Wellspring, print a report, and set the
//!   pytest-style exit code. Needs `TIDERACE_SHIM` (path to `shim.py`); `TIDERACE_PYTHON` defaults
//!   to `python3`.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use engine_core::collection::{Collector, RegexCollector};
use engine_core::domain::{Outcome, RunReport};
use engine_core::exec::{ForkWorker, Worker};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: tiderace <collect|run> <path>");
        return ExitCode::from(64);
    }
    let root = PathBuf::from(&args[2]);
    match args[1].as_str() {
        "collect" => cmd_collect(&root),
        "run" => cmd_run(&root),
        other => {
            eprintln!("unknown command: {other}");
            ExitCode::from(64)
        }
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

fn cmd_run(root: &Path) -> ExitCode {
    let python = std::env::var("TIDERACE_PYTHON").unwrap_or_else(|_| "python3".to_string());
    let shim = match std::env::var("TIDERACE_SHIM") {
        Ok(s) => PathBuf::from(s),
        Err(_) => {
            eprintln!("error: set TIDERACE_SHIM to the path of py-shim/shim.py");
            return ExitCode::FAILURE;
        }
    };

    let collector = RegexCollector::new();
    let items = match collector.collect(root) {
        Ok(items) => items,
        Err(e) => {
            eprintln!("error: collection failed: {e}");
            return ExitCode::FAILURE;
        }
    };

    let mut worker = match ForkWorker::launch(&python, &shim, root) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let results = match worker.run(&items) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    let report = RunReport::new(results);
    for result in &report.results {
        println!("{}\t{}", label(result.outcome), result.node_id);
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
