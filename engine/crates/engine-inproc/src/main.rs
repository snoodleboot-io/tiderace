//! ② backend proof: drive a real corpus through [`InProcessTransport`] — one embedded interpreter,
//! imported once, fork-from-embedded per test, results returned by FFI as `ExecResponse` Rust values
//! (no subprocess, no pipe/JSON control plane). **No pytest.**

use std::path::PathBuf;
use std::time::Instant;

use engine_core::collection::{Collector, RegexCollector};
use engine_core::exec::{ExecRequest, ShimTransport};
use engine_inproc::{engine_py_paths, InProcessTransport};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    // `inproc-probe bench <corpus> [iters]` — time a full corpus through the in-process backend.
    if args.get(1).map(String::as_str) == Some("bench") {
        std::process::exit(bench(&args));
    }
    // `inproc-probe smart <corpus>` — learn purity, then re-run routing pure→no-fork vs all-fork.
    if args.get(1).map(String::as_str) == Some("smart") {
        std::process::exit(smart(&args));
    }
    proof();
}

/// The realistic pure-batching flow: phase 1 learns each test's purity (forked, safe); phase 2 re-runs
/// routing **pure tests to no-fork** and forking only impure ones. Compared to an all-fork re-run.
fn smart(args: &[String]) -> i32 {
    let corpus = PathBuf::from(args.get(2).expect("usage: inproc-probe smart <corpus>"));
    let engine_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let items = RegexCollector::new().collect(&corpus).expect("collect");
    let mut t = InProcessTransport::new(&corpus, engine_py_paths(&engine_dir), false)
        .with_purity_guard(true);
    t.ready().expect("ready");

    // Phase 1 — learn purity (forked, safe).
    let learn = Instant::now();
    let mut pure = std::collections::HashSet::new();
    let mut passed = 0;
    for it in &items {
        let (outcome, p) = t.run_node(it.node_id.as_str(), it.style.wire(), 5000, false).unwrap();
        if outcome == "passed" {
            passed += 1;
        }
        if p == Some(true) {
            pure.insert(it.node_id.to_string());
        }
    }
    let learn_ms = learn.elapsed().as_secs_f64() * 1000.0;
    println!(
        "[learn ] {} tests ({} passed, {} pure / {} impure) forked in {learn_ms:.0} ms",
        items.len(),
        passed,
        pure.len(),
        items.len() - pure.len()
    );

    // Phase 2a — all-fork re-run (baseline).
    let all_fork = time_run(&t, &items, |_| false);
    // Phase 2b — smart: pure→no-fork, impure→fork.
    let smart = time_run(&t, &items, |n| pure.contains(n));
    println!("[re-run] all-fork : {all_fork:.0} ms");
    println!("[re-run] smart    : {smart:.0} ms   ({:.1}× faster, {} of {} tests no-fork)",
        all_fork / smart.max(0.001), pure.len(), items.len());
    0
}

fn time_run(
    t: &InProcessTransport,
    items: &[engine_core::domain::TestItem],
    no_fork: impl Fn(&str) -> bool,
) -> f64 {
    let started = Instant::now();
    for it in items {
        let nf = no_fork(it.node_id.as_str());
        t.run_node(it.node_id.as_str(), it.style.wire(), 5000, nf).unwrap();
    }
    started.elapsed().as_secs_f64() * 1000.0
}

/// Time `iters` full passes of a real corpus through the embedded interpreter (import-once + per-test
/// fork-from-embedded), to compare against the subprocess `PipeTransport` baseline.
fn bench(args: &[String]) -> i32 {
    let corpus = PathBuf::from(args.get(2).expect("usage: inproc-probe bench <corpus> [iters]"));
    let iters: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(3);
    let engine_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();

    let items = RegexCollector::new().collect(&corpus).expect("collect");

    // Profile: same tests, fork-per-test (isolated) vs in-process (no fork) — isolates the fork cost.
    for (label, no_fork) in [("fork-per-test (isolated)", false), ("no-fork (in-process)", true)] {
        let mut transport =
            InProcessTransport::new(&corpus, engine_py_paths(&engine_dir), false).with_no_fork(no_fork);
        let boot = Instant::now();
        transport.ready().expect("ready");
        let boot_ms = boot.elapsed().as_secs_f64() * 1000.0;
        let mut last = 0.0;
        let mut passed = 0usize;
        for _ in 0..iters {
            let started = Instant::now();
            passed = 0;
            for it in &items {
                let resp = transport
                    .exchange(&ExecRequest::bare(it.node_id.as_str(), it.style.wire(), 5000))
                    .expect("exchange");
                if resp.outcome == "passed" {
                    passed += 1;
                }
            }
            last = started.elapsed().as_secs_f64() * 1000.0;
        }
        println!(
            "[{label:<26}] {} tests ({} passed) in {last:.1} ms ({:.2} ms/test)  +{boot_ms:.0} ms import",
            items.len(),
            passed,
            last / items.len() as f64,
        );
    }
    0
}

fn proof() {
    let dir = std::env::temp_dir().join(format!("inproc_probe_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("test_x.py"),
        "_STATE = {'n': 0}\n\
         def test_ok():\n    assert 1 + 1 == 2\n\
         def test_bad():\n    assert 1 == 2\n\
         def test_upper():\n    assert 'ab'.upper() == 'AB'\n\
         def test_mutate():\n    _STATE['n'] += 1\n    assert _STATE['n'] == 1\n\
         def test_isolated():\n    assert _STATE['n'] == 0  # passes ONLY if test_mutate ran in a forked child\n",
    )
    .unwrap();

    let engine_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();

    let mut transport = InProcessTransport::new(&dir, engine_py_paths(&engine_dir), false);
    let ready = transport.ready().expect("ready");
    println!(
        "[ready] in-process wellspring pid={} (one embedded interpreter, project imported once)",
        ready.pid
    );

    let expected = [
        ("test_ok", "passed"),
        ("test_bad", "failed"),
        ("test_upper", "passed"),
        ("test_mutate", "passed"),   // mutates a module global in its forked child
        ("test_isolated", "passed"), // sees a clean global ⇒ the mutation did NOT leak ⇒ fork isolated
    ];
    let mut ok = true;
    for (name, want) in expected {
        let node = format!("test_x.py::{name}");
        let resp = transport
            .exchange(&ExecRequest::bare(&node, "function", 5000))
            .expect("exchange");
        let mark = if resp.outcome == want {
            "ok"
        } else {
            ok = false;
            "!!"
        };
        println!("[run] {name:<12} {:<8} {mark}", resp.outcome);
    }

    println!(
        "\n=== VERDICT: {} ===",
        if ok {
            "GO — InProcessTransport drives tests by FFI (fork-from-embedded, no subprocess/pipe)"
        } else {
            "NO-GO"
        }
    );
    let _ = std::fs::remove_dir_all(&dir);
    std::process::exit(if ok { 0 } else { 1 });
}
