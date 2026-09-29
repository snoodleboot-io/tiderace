//! TID-84 — `tiderace daemon start|status|stop` and `tiderace run` through a live daemon.
//!
//! `status` and `stop` against a root nobody serves say so and fail; with a daemon serving the
//! root, `run` reports "via daemon" and the daemon's results, `-k` stays local, and `stop` ends the
//! server. `start` spawns the daemon binary beside this one and waits for it to answer.

#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repo root")
}

fn shim() -> PathBuf {
    repo_root().join("engine/py-shim/shim.py")
}

fn any_python() -> Option<String> {
    let venv = repo_root().join(".tiderace-fx-venv/bin/python");
    if venv.exists() {
        return Some(venv.to_string_lossy().into_owned());
    }
    ["python3", "python"]
        .into_iter()
        .find(|cand| {
            Command::new(cand)
                .arg("--version")
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        })
        .map(str::to_string)
}

fn write_project(tag: &str) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "tiderace_t84_cli_{tag}_{}_{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("test_plain.py"),
        "def test_one():\n    assert True\n\ndef test_two():\n    assert 1 == 2\n",
    )
    .unwrap();
    dir.canonicalize().unwrap()
}

fn tiderace(python: &str) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_tiderace"));
    cmd.env("TIDERACE_PYTHON", python)
        .env("TIDERACE_SHIM", shim());
    cmd
}

fn text(out: &std::process::Output) -> (String, String) {
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn status_and_stop_without_a_daemon_say_so_and_fail() {
    let Some(python) = any_python() else {
        eprintln!("skipping: no Python interpreter available");
        return;
    };
    let dir = write_project("none");
    for verb in ["status", "stop"] {
        let out = tiderace(&python)
            .arg("daemon")
            .arg(verb)
            .arg(&dir)
            .output()
            .unwrap();
        let (stdout, _) = text(&out);
        assert!(!out.status.success(), "{verb} without a daemon fails");
        assert!(stdout.contains("no daemon serving"), "{verb}: {stdout}");
    }
    let out = tiderace(&python)
        .arg("daemon")
        .arg("status")
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(64),
        "a missing path is a usage error"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A daemon served in-process (the library the binary is glue over), so the CLI's client side is
/// proven without depending on a second binary being built.
#[test]
fn run_goes_through_a_serving_daemon_and_stop_ends_it() {
    let Some(python) = any_python() else {
        eprintln!("skipping: no Python interpreter available");
        return;
    };
    let dir = write_project("serve");
    let socket = engine_daemon::daemon_socket_path(&dir);
    let server = {
        let (python, dir) = (python.clone(), dir.clone());
        std::thread::spawn(move || {
            let mut handler = engine_daemon::EngineHandler::new(python, shim(), dir);
            engine_daemon::serve_unix_socket(&socket, &mut handler)
        })
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let out = tiderace(&python)
            .arg("daemon")
            .arg("status")
            .arg(&dir)
            .output()
            .unwrap();
        if out.status.success() {
            let (stdout, _) = text(&out);
            assert!(stdout.contains("daemon serving"), "{stdout}");
            assert!(stdout.contains("cold"), "nothing has run yet: {stdout}");
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the daemon never answered"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    // Two full runs: the second is served from the warm image. Both report as a local run would.
    for round in 1..=2 {
        let report = dir.join(format!("report{round}.json"));
        let out = tiderace(&python)
            .arg("run")
            .arg("--report")
            .arg(&report)
            .arg(&dir)
            .output()
            .unwrap();
        let (stdout, stderr) = text(&out);
        assert!(stderr.contains("via daemon"), "run {round}: {stderr}");
        assert_eq!(
            out.status.code(),
            Some(1),
            "one failing test: {stdout}\n{stderr}"
        );
        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&report).unwrap()).unwrap();
        let results = json["tests"].as_array().expect("tests");
        assert_eq!(results.len(), 2, "run {round}: {json}");
    }
    let out = tiderace(&python)
        .arg("daemon")
        .arg("status")
        .arg(&dir)
        .output()
        .unwrap();
    assert!(text(&out).0.contains("image warm"), "{}", text(&out).0);

    // A filtered run stays in this process.
    let out = tiderace(&python)
        .arg("run")
        .arg("-k")
        .arg("test_one")
        .arg(&dir)
        .output()
        .unwrap();
    let (_, stderr) = text(&out);
    assert!(!stderr.contains("via daemon"), "-k runs locally: {stderr}");
    assert!(out.status.success(), "{stderr}");

    // So does one under TIDERACE_NO_DAEMON — and it reports the same nodes and outcomes.
    let local = dir.join("local.json");
    let out = tiderace(&python)
        .env("TIDERACE_NO_DAEMON", "1")
        .arg("run")
        .arg("--report")
        .arg(&local)
        .arg(&dir)
        .output()
        .unwrap();
    let (_, stderr) = text(&out);
    assert!(!stderr.contains("via daemon"), "{stderr}");
    let outcomes = |path: &PathBuf| -> Vec<(String, String)> {
        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let mut v: Vec<(String, String)> = json["tests"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| {
                (
                    t["node_id"].as_str().unwrap().to_string(),
                    t["outcome"].as_str().unwrap().to_string(),
                )
            })
            .collect();
        v.sort();
        v
    };
    assert_eq!(
        outcomes(&dir.join("report2.json")),
        outcomes(&local),
        "daemon vs local"
    );

    let out = tiderace(&python)
        .arg("daemon")
        .arg("stop")
        .arg(&dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", text(&out).0);
    server.join().unwrap().expect("the server returned cleanly");
    let out = tiderace(&python)
        .arg("daemon")
        .arg("status")
        .arg(&dir)
        .output()
        .unwrap();
    assert!(!out.status.success(), "stopped: {}", text(&out).0);
    let _ = std::fs::remove_dir_all(&dir);
}

/// `daemon start` spawns the daemon binary and waits for it. Needs `tiderace-daemon` built beside
/// this binary (a workspace `cargo test` does that); otherwise the scenario is skipped.
#[test]
fn start_spawns_the_daemon_binary_and_waits_for_it() {
    let Some(python) = any_python() else {
        eprintln!("skipping: no Python interpreter available");
        return;
    };
    let daemon_bin =
        PathBuf::from(env!("CARGO_BIN_EXE_tiderace")).with_file_name("tiderace-daemon");
    if !daemon_bin.exists() {
        eprintln!("skipping: {} is not built", daemon_bin.display());
        return;
    }
    let dir = write_project("start");
    let out = tiderace(&python)
        .env("TIDERACE_DAEMON_BIN", &daemon_bin)
        .arg("daemon")
        .arg("start")
        .arg(&dir)
        .output()
        .unwrap();
    let (stdout, stderr) = text(&out);
    assert!(out.status.success(), "{stdout}\n{stderr}");
    assert!(stdout.contains("daemon started"), "{stdout}");
    assert!(dir.join(".tiderace-cache/daemon.log").exists());

    let out = tiderace(&python)
        .arg("daemon")
        .arg("start")
        .arg(&dir)
        .output()
        .unwrap();
    assert!(text(&out).0.contains("already serving"), "{}", text(&out).0);

    let out = tiderace(&python).arg("run").arg(&dir).output().unwrap();
    let (_, stderr) = text(&out);
    assert!(stderr.contains("via daemon"), "{stderr}");
    assert_eq!(out.status.code(), Some(1), "{stderr}");

    let out = tiderace(&python)
        .arg("daemon")
        .arg("stop")
        .arg(&dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", text(&out).0);
    let _ = std::fs::remove_dir_all(&dir);
}
