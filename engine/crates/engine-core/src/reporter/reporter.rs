use crate::domain::{Outcome, RunReport};

/// The reporter seam (ADR-E005, design 13): render a finished [`RunReport`] into one output format.
/// Implementors return the rendered text; *where* it goes (stdout, a file) is the caller's choice, so
/// reporters stay pure and unit-testable against their schema/consumer.
pub trait Reporter {
    /// Render the run into this reporter's format.
    fn render(&self, report: &RunReport) -> String;
}

/// The stable wire/display token for an outcome (shared by every reporter so formats agree).
pub(crate) fn outcome_token(outcome: Outcome) -> &'static str {
    match outcome {
        Outcome::Passed => "passed",
        Outcome::Failed => "failed",
        Outcome::Skipped => "skipped",
        Outcome::XFail => "xfail",
        Outcome::XPass => "xpass",
        Outcome::Error => "error",
    }
}
