use serde::Serialize;

use crate::domain::{Outcome, RunReport};
use crate::reporter::Reporter;

/// SARIF 2.1.0 reporter (design 13) — the OASIS standard code-scanning consumers ingest (GitHub
/// code-scanning, IDEs). Each failing/erroring test becomes a `result` with a physical location, so a
/// test failure shows up alongside lints/security findings in the same pane.
#[derive(Debug, Default, Clone, Copy)]
pub struct SarifReporter;

impl Reporter for SarifReporter {
    fn render(&self, report: &RunReport) -> String {
        let results: Vec<SarifResult> = report
            .results
            .iter()
            .filter(|r| matches!(r.outcome, Outcome::Failed | Outcome::Error))
            .map(|r| SarifResult {
                rule_id: "riptide.test-failure",
                level: "error", // both Failed and Error map to SARIF level "error"
                message: SarifMessage { text: &r.detail },
                locations: vec![SarifLocation {
                    physical_location: SarifPhysical {
                        artifact_location: SarifArtifact {
                            uri: r.node_id.file(),
                        },
                    },
                }],
            })
            .collect();

        let doc = SarifLog {
            schema: "https://json.schemastore.org/sarif-2.1.0.json",
            version: "2.1.0",
            runs: vec![SarifRun {
                tool: SarifTool {
                    driver: SarifDriver {
                        name: "riptide",
                        rules: vec![SarifRule {
                            id: "riptide.test-failure",
                        }],
                    },
                },
                results,
            }],
        };
        serde_json::to_string_pretty(&doc).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
    }
}

#[derive(Serialize)]
struct SarifLog<'a> {
    #[serde(rename = "$schema")]
    schema: &'a str,
    version: &'a str,
    runs: Vec<SarifRun<'a>>,
}
#[derive(Serialize)]
struct SarifRun<'a> {
    tool: SarifTool<'a>,
    results: Vec<SarifResult<'a>>,
}
#[derive(Serialize)]
struct SarifTool<'a> {
    driver: SarifDriver<'a>,
}
#[derive(Serialize)]
struct SarifDriver<'a> {
    name: &'a str,
    rules: Vec<SarifRule<'a>>,
}
#[derive(Serialize)]
struct SarifRule<'a> {
    id: &'a str,
}
#[derive(Serialize)]
struct SarifResult<'a> {
    #[serde(rename = "ruleId")]
    rule_id: &'a str,
    level: &'a str,
    message: SarifMessage<'a>,
    locations: Vec<SarifLocation<'a>>,
}
#[derive(Serialize)]
struct SarifMessage<'a> {
    text: &'a str,
}
#[derive(Serialize)]
struct SarifLocation<'a> {
    #[serde(rename = "physicalLocation")]
    physical_location: SarifPhysical<'a>,
}
#[derive(Serialize)]
struct SarifPhysical<'a> {
    #[serde(rename = "artifactLocation")]
    artifact_location: SarifArtifact<'a>,
}
#[derive(Serialize)]
struct SarifArtifact<'a> {
    uri: &'a str,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{NodeId, TestResult};

    #[test]
    fn emits_valid_sarif_with_one_result_per_failure() {
        let report = RunReport::new(vec![
            TestResult::new(NodeId::new("t.py::a"), Outcome::Passed, 1, ""),
            TestResult::new(NodeId::new("t.py::b"), Outcome::Failed, 2, "boom"),
        ]);
        let sarif = SarifReporter.render(&report);
        let v: serde_json::Value = serde_json::from_str(&sarif).expect("valid JSON");
        assert_eq!(v["version"], "2.1.0");
        let results = v["runs"][0]["results"].as_array().unwrap();
        assert_eq!(results.len(), 1, "only failures become results");
        assert_eq!(results[0]["message"]["text"], "boom");
        assert_eq!(
            results[0]["locations"][0]["physicalLocation"]["artifactLocation"]["uri"],
            "t.py"
        );
    }
}
