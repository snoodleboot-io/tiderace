use serde::Serialize;

use crate::domain::{Outcome, RunReport, TestResult};
use crate::reporter::reporter::outcome_token;
use crate::reporter::Reporter;

/// JSON reporter — a machine-readable run summary for dashboards, bots, and `--report` consumers.
/// Stable shape: tallies + the per-test results. Built on `serde_json` (already a workspace dep).
///
/// Per-test records carry the node id, so a consumer compares *sets of ids* rather than tallies.
/// That distinction is the whole point of the format (TID-55): a 62-test gap against pytest once
/// decomposed into 36 changed outcomes plus 32 ids that existed on one side only plus 6 extra —
/// two opposite errors partly cancelling inside one number, which no tally could have shown.
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
    /// Distinct modules that skipped at import — the other dimension of `skipped` (TID-55).
    skipped_modules: usize,
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
    /// The module whose import skipped this test; absent for a per-test skip.
    #[serde(skip_serializing_if = "str::is_empty")]
    skip_origin: &'a str,
    /// Produced by runtime expansion (a parametrize case, an inherited method) rather than collection.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    expanded: bool,
    /// Measured purity verdict; absent when the tier ran did not measure one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pure: Option<bool>,
    /// This test disturbed interpreter state nothing undid, so later runs fork it from the start.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    must_fork: bool,
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
            skipped_modules: report.skipped_modules(),
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
        skip_origin: &r.skip_origin,
        expanded: r.expanded,
        pure: r.pure,
        must_fork: r.must_fork,
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
