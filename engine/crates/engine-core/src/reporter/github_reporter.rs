use crate::domain::{Outcome, RunReport};
use crate::reporter::Reporter;

/// GitHub Actions annotations reporter (design 13): emits `::error`/`::warning` workflow commands so
/// failures surface inline on the PR's Files-changed view and in the run log. Failures/errors become
/// `::error`, an unexpected pass (`xpass`) a `::warning`.
#[derive(Debug, Default, Clone, Copy)]
pub struct GithubReporter;

impl Reporter for GithubReporter {
    fn render(&self, report: &RunReport) -> String {
        let mut out = String::new();
        for r in &report.results {
            let level = match r.outcome {
                Outcome::Failed | Outcome::Error => "error",
                Outcome::XPass => "warning",
                _ => continue,
            };
            let file = r.node_id.file();
            out.push_str(&format!(
                "::{level} file={},title={}::{}\n",
                prop(file),
                prop(r.node_id.as_str()),
                data(&r.detail),
            ));
        }
        out
    }
}

/// Escape a workflow-command **property** value (GitHub requires %25/%0A/%0D and %2C/%3A in properties).
fn prop(s: &str) -> String {
    data(s).replace(',', "%2C").replace(':', "%3A")
}

/// Escape a workflow-command **message/data** value.
fn data(s: &str) -> String {
    s.replace('%', "%25")
        .replace('\r', "%0D")
        .replace('\n', "%0A")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{NodeId, TestResult};

    #[test]
    fn emits_error_annotation_for_failures_with_escaping() {
        let report = RunReport::new(vec![
            TestResult::new(NodeId::new("pkg/test_m.py::C::a"), Outcome::Passed, 1, ""),
            TestResult::new(
                NodeId::new("pkg/test_m.py::C::b"),
                Outcome::Failed,
                2,
                "line1\nline2",
            ),
        ]);
        let out = GithubReporter.render(&report);
        assert!(out.contains("::error file=pkg/test_m.py,"));
        assert!(out.contains("title=pkg/test_m.py%3A%3AC%3A%3Ab")); // colons escaped in property
        assert!(out.contains("line1%0Aline2")); // newline escaped in message
        assert_eq!(out.lines().count(), 1, "only the failing test is annotated");
    }
}
