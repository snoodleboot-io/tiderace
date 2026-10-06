//! `tiderace collect` and `tiderace run`: the target, the route, the plan that actually runs,
//! and what `run` writes back (TID-120).

use std::path::Path;
use std::process::ExitCode;

use engine_core::collection::{Collector, RegexCollector};
use engine_core::domain::RunReport;
use engine_core::runner::{record_hints, Learned, RunPlan, VerdictStore, WorkerCount};
use engine_daemon::DaemonClient;

use crate::args::{Options, Route};
use crate::report;

/// `tiderace collect <path>`.
pub fn collect(root: &Path) -> ExitCode {
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
pub fn effective_plan(plan: &RunPlan, item_count: usize, root: &Path) -> (RunPlan, Learned) {
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
    let learned = Learned {
        trusted_pure: Default::default(),
        must_fork,
        durations,
    };
    let effective = RunPlan {
        workers: if plan.workers.is_explicit() {
            WorkerCount::Explicit(plan.effective_workers(item_count))
        } else {
            WorkerCount::Default(plan.effective_workers(item_count))
        },
        ..plan.clone()
    };
    (effective, learned)
}

/// `tiderace run`: through the daemon serving the root when the route allows and one is
/// listening, else in this process; then the report and pytest's exit code.
pub fn execute(opts: Options) -> ExitCode {
    let Options {
        root,
        plan,
        quiet,
        selection,
        report: report_path,
        route,
    } = opts;
    let (root, plan, report_path) = (root.as_path(), &plan, report_path.as_deref());
    // Handed to the shim through the environment: it is the process that reads the project's
    // own `addopts`, and it applies the same precedence pytest does — an expression on the
    // command line wins over one in the config (TID-59); strict markers take the same route
    // (TID-67), since a project with no config file has nowhere else to say it. Only what is
    // set is written: an absent `-k` leaves whatever the environment already carries.
    // SAFETY: single-threaded here; workers are spawned further down, and the guard lives
    // until the run is over.
    let _selection_env = unsafe { selection.apply_env_set_only() };
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
    let via_daemon = match route {
        Route::Daemon => DaemonClient::for_root(root).run_full(&selection),
        Route::Local => None,
    };
    timing.mark("daemon round trip");
    let results = match via_daemon {
        Some(Ok(results)) => {
            let (effective, learned) = effective_plan(plan, results.len(), root);
            eprintln!("tiderace: {} via daemon", effective.header_with(&learned));
            results
        }
        Some(Err(e)) => {
            eprintln!("error: the daemon could not run this: {e}");
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
            eprintln!("tiderace: {}", effective.header_with(&learned));

            // `&effective`, not `plan`: the header and the run must describe the same thing. They
            // did not, so the worker clamp shown in the header was never the clamp applied — and
            // the verdicts read above would have been reported and then dropped on the floor.
            let results = match engine_core::runner::run_parallel_with_notes(
                &python, &shim, root, items, &effective, &learned,
            ) {
                Ok(outcome) => {
                    for line in &outcome.notes.lines {
                        eprintln!("tiderace: {line}");
                    }
                    outcome.results
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    return ExitCode::FAILURE;
                }
            };

            // What `run` writes back: how long each node took (TID-62, ADR-E016), so the next
            // run's scheduler hands out the heaviest module first instead of the one with the most
            // tests, and which nodes disturbed interpreter state (TID-127), so the next run forks
            // them from the start instead of paying an in-process attempt and a clean-room re-run
            // to find out again. Hints, not verdicts — the verdict store's "reading only" contract
            // still holds for everything that can change an answer — and best-effort: a tree that
            // cannot be written runs cold next time, which is not a failure of this run.
            if let Err(e) = record_hints(root, &results) {
                eprintln!(
                    "warning: could not record run hints in {}: {e}",
                    root.display()
                );
            }
            results
        }
    };
    let report = RunReport::new(results);
    report::print(&report, quiet);
    report::write_json(&report, report_path);
    ExitCode::from(report.exit_code() as u8)
}
