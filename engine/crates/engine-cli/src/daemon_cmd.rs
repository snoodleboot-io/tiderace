//! `tiderace daemon start|status|stop <path>` over the daemon's own client (TID-84, TID-120).

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use engine_daemon::DaemonClient;

use crate::args::DaemonVerb;

pub fn execute(verb: DaemonVerb, root: &Path) -> ExitCode {
    let client = DaemonClient::for_root(root);
    let root = client.root();
    match verb {
        DaemonVerb::Status => match client.health() {
            Some(h) => {
                println!(
                    "daemon serving {} — pid {}, image {}",
                    root.display(),
                    h.pid,
                    if h.warm {
                        "warm"
                    } else {
                        "cold (the first run imports the suite)"
                    }
                );
                ExitCode::SUCCESS
            }
            None => {
                println!("no daemon serving {}", root.display());
                ExitCode::FAILURE
            }
        },
        DaemonVerb::Stop => {
            if client.shutdown() {
                println!("daemon for {} stopped", root.display());
                ExitCode::SUCCESS
            } else {
                println!("no daemon serving {}", root.display());
                ExitCode::FAILURE
            }
        }
        DaemonVerb::Start => start(&client),
    }
}

fn start(client: &DaemonClient) -> ExitCode {
    let root = client.root();
    if let Some(h) = client.health() {
        println!(
            "a daemon is already serving {} (pid {})",
            root.display(),
            h.pid
        );
        return ExitCode::SUCCESS;
    }
    let log = root.join(".tiderace-cache").join("daemon.log");
    match client.start(&daemon_binary(), &log) {
        Ok(started) => {
            println!(
                "daemon started for {} (pid {}): the first run imports the suite, later runs do \
                 not. Log: {}",
                root.display(),
                started.pid,
                started.log.display()
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// The daemon binary ships beside this one; `TIDERACE_DAEMON_BIN` points elsewhere.
fn daemon_binary() -> PathBuf {
    std::env::var("TIDERACE_DAEMON_BIN")
        .map(PathBuf::from)
        .or_else(|_| {
            std::env::current_exe().map(|exe| {
                exe.with_file_name(format!("tiderace-daemon{}", std::env::consts::EXE_SUFFIX))
            })
        })
        .unwrap_or_else(|_| PathBuf::from("tiderace-daemon"))
}
