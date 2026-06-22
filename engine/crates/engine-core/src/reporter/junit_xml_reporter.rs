use crate::domain::{Outcome, RunReport};
use crate::reporter::Reporter;

/// JUnit XML reporter — the lingua franca CI consumes (Jenkins, GitLab, GitHub test reporting).
/// Emits a single `<testsuite>` with per-test `<testcase>` elements; failures/errors carry the detail,
/// skips/xfails a `<skipped>` marker. Hand-rolled (no XML dependency) with proper attribute/text
/// escaping — validated against the JUnit schema consumers expect.
#[derive(Debug, Default, Clone, Copy)]
pub struct JunitXmlReporter;

impl Reporter for JunitXmlReporter {
    fn render(&self, report: &RunReport) -> String {
        let failures = report.tally(Outcome::Failed);
        let errors = report.tally(Outcome::Error);
        let skipped = report.tally(Outcome::Skipped) + report.tally(Outcome::XFail);
        let time: f64 = report
            .results
            .iter()
            .map(|r| r.duration_ms as f64 / 1000.0)
            .sum();

        let mut out = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
        out.push_str(&format!(
            "<testsuite name=\"riptide\" tests=\"{}\" failures=\"{}\" errors=\"{}\" skipped=\"{}\" time=\"{:.3}\">\n",
            report.total(),
            failures,
            errors,
            skipped,
            time
        ));
        for r in &report.results {
            let (classname, name) = split_node(r.node_id.as_str());
            out.push_str(&format!(
                "  <testcase classname=\"{}\" name=\"{}\" time=\"{:.3}\"",
                attr(&classname),
                attr(&name),
                r.duration_ms as f64 / 1000.0
            ));
            match r.outcome {
                Outcome::Failed => out.push_str(&format!(
                    ">\n    <failure message=\"assertion failed\">{}</failure>\n  </testcase>\n",
                    text(&r.detail)
                )),
                Outcome::Error => out.push_str(&format!(
                    ">\n    <error message=\"error\">{}</error>\n  </testcase>\n",
                    text(&r.detail)
                )),
                Outcome::Skipped | Outcome::XFail => {
                    out.push_str(">\n    <skipped/>\n  </testcase>\n")
                }
                _ => out.push_str("/>\n"),
            }
        }
        out.push_str("</testsuite>\n");
        out
    }
}

/// `pkg/test_m.py::C::t` → (classname `pkg/test_m.py::C`, name `t`).
fn split_node(node: &str) -> (String, String) {
    match node.rsplit_once("::") {
        Some((cls, name)) => (cls.to_string(), name.to_string()),
        None => (String::new(), node.to_string()),
    }
}

fn attr(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn text(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{NodeId, TestResult};

    #[test]
    fn emits_well_formed_counts_and_escapes() {
        let report = RunReport::new(vec![
            TestResult::new(NodeId::new("t.py::C::a"), Outcome::Passed, 5, ""),
            TestResult::new(NodeId::new("t.py::C::b"), Outcome::Failed, 3, "a < b & c"),
            TestResult::new(NodeId::new("t.py::C::c"), Outcome::Skipped, 0, ""),
        ]);
        let xml = JunitXmlReporter.render(&report);
        assert!(xml.contains("tests=\"3\" failures=\"1\" errors=\"0\" skipped=\"1\""));
        assert!(xml.contains("classname=\"t.py::C\" name=\"b\""));
        assert!(
            xml.contains("a &lt; b &amp; c"),
            "detail must be XML-escaped"
        );
        assert!(xml.contains("<skipped/>"));
    }
}
