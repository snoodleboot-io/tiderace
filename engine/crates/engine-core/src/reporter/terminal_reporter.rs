use crate::domain::{Outcome, RunReport};
use crate::reporter::reporter::outcome_token;
use crate::reporter::Reporter;

/// The default human reporter: a one-line tally plus the detail of every failing/erroring test.
/// (Color/TTY handling is a presentation concern layered on top; this renders plain, testable text.)
#[derive(Debug, Default, Clone, Copy)]
pub struct TerminalReporter;

impl Reporter for TerminalReporter {
    fn render(&self, report: &RunReport) -> String {
        let mut out = String::new();
        for r in &report.results {
            if matches!(r.outcome, Outcome::Failed | Outcome::Error) {
                out.push_str(&format!(
                    "{} {}\n{}\n",
                    outcome_token(r.outcome).to_uppercase(),
                    r.node_id,
                    indent(&r.detail)
                ));
            }
        }
        out.push_str(&summary_line(report));
        out
    }
}

fn summary_line(report: &RunReport) -> String {
    let parts = [
        Outcome::Passed,
        Outcome::Failed,
        Outcome::Error,
        Outcome::Skipped,
        Outcome::XFail,
        Outcome::XPass,
    ]
    .into_iter()
    .filter_map(|o| {
        let n = report.tally(o);
        (n > 0).then(|| format!("{n} {}", outcome_token(o)))
    })
    .collect::<Vec<_>>();
    let body = if parts.is_empty() {
        "no tests".to_string()
    } else {
        parts.join(", ")
    };
    format!("=== {body} ({} total) ===", report.total())
}

fn indent(detail: &str) -> String {
    detail
        .lines()
        .map(|l| format!("    {l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{NodeId, TestResult};

    fn report() -> RunReport {
        RunReport::new(vec![
            TestResult::new(NodeId::new("t.py::a"), Outcome::Passed, 1, ""),
            TestResult::new(NodeId::new("t.py::b"), Outcome::Failed, 2, "assert 1 == 2"),
        ])
    }

    #[test]
    fn shows_tally_and_failure_detail() {
        let out = TerminalReporter.render(&report());
        assert!(out.contains("FAILED t.py::b"));
        assert!(out.contains("assert 1 == 2"));
        assert!(out.contains("1 passed, 1 failed (2 total)"));
    }
}
