//! What `tiderace run` prints: the per-test lines, the tally with its import-skip note, the
//! memory note (TID-106), and the optional JSON report (TID-120). Byte-for-byte what `cmd_run`
//! printed before the split: `--report` consumers and the acceptance tests compare it.

use std::path::Path;

use engine_core::domain::{Outcome, RunReport};
use engine_core::reporter::{JsonReporter, Reporter};

/// The per-test lines (unless `quiet`), then the tally and the memory note on stderr.
pub fn print(report: &RunReport, quiet: bool) {
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
}

/// The machine-readable report, if one was asked for.
pub fn write_json(report: &RunReport, report_path: Option<&Path>) {
    if let Some(path) = report_path {
        // Written after the summary so a failure to write is the last thing on the terminal, and
        // non-fatal: the run's own verdict is what the exit code is for, and losing the report file
        // must not turn a green suite red.
        if let Err(e) = std::fs::write(path, JsonReporter.render(report)) {
            eprintln!("warning: could not write --report {}: {e}", path.display());
        }
    }
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
