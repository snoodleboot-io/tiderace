use serde::Serialize;

use crate::domain::{Outcome, RunReport, TestResult};
use crate::reporter::reporter::outcome_token;
use crate::reporter::Reporter;

/// JSON reporter — a machine-readable run summary for dashboards, bots, and `--json` consumers.
/// Stable shape: tallies + the per-test results. Built on `serde_json` (already a workspace dep).
#[derive(Debug, Default, Clone, Copy)]
pub struct JsonReporter;

#[derive(Serialize)]
struct JsonRun<'a> {
    total: usize,
    passed: usize,
    failed: usize,
    errored: usize,
    skipped: usize,
    xfailed: usize,
    xpassed: usize,
    exit_code: i32,
    tests: Vec<JsonTest<'a>>,
}

#[derive(Serialize)]
struct JsonTest<'a> {
    node_id: &'a str,
    outcome: &'static str,
    duration_ms: u64,
    #[serde(skip_serializing_if = "str::is_empty")]
    detail: &'a str,
}

impl Reporter for JsonReporter {
    fn render(&self, report: &RunReport) -> String {
        let view = JsonRun {
            total: report.total(),
            passed: report.tally(Outcome::Passed),
            failed: report.tally(Outcome::Failed),
            errored: report.tally(Outcome::Error),
            skipped: report.tally(Outcome::Skipped),
            xfailed: report.tally(Outcome::XFail),
            xpassed: report.tally(Outcome::XPass),
            exit_code: report.exit_code(),
            tests: report.results.iter().map(json_test).collect(),
        };
        serde_json::to_string_pretty(&view).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
    }
}

fn json_test(r: &TestResult) -> JsonTest<'_> {
    JsonTest {
        node_id: r.node_id.as_str(),
        outcome: outcome_token(r.outcome),
        duration_ms: r.duration_ms,
        detail: &r.detail,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::NodeId;

    #[test]
    fn emits_valid_json_with_tallies() {
        let report = RunReport::new(vec![
            TestResult::new(NodeId::new("t.py::a"), Outcome::Passed, 1, ""),
            TestResult::new(NodeId::new("t.py::b"), Outcome::Failed, 2, "boom"),
        ]);
        let json = JsonReporter.render(&report);
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        assert_eq!(parsed["total"], 2);
        assert_eq!(parsed["passed"], 1);
        assert_eq!(parsed["failed"], 1);
        assert_eq!(parsed["exit_code"], 1);
        assert_eq!(parsed["tests"][1]["outcome"], "failed");
        assert_eq!(parsed["tests"][1]["detail"], "boom");
    }
}
