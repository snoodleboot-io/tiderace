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

mod args;
mod daemon_cmd;
mod report;
mod run;

use std::process::ExitCode;

use args::{Command, USAGE};

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    match args::parse(&argv) {
        Ok(Command::Help { no_args }) => {
            println!("{USAGE}");
            if no_args {
                ExitCode::from(64)
            } else {
                ExitCode::SUCCESS
            }
        }
        Ok(Command::Collect { root }) => run::collect(&root),
        Ok(Command::Run(opts)) => run::execute(*opts),
        Ok(Command::Daemon { verb, root }) => daemon_cmd::execute(verb, &root),
        Err(msg) => {
            eprintln!("error: {msg}\n\n{USAGE}");
            ExitCode::from(64)
        }
    }
}
