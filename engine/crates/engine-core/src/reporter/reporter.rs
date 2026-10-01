use crate::domain::RunReport;

/// The reporter seam (ADR-E005, design 13): render a finished [`RunReport`] into one output format.
/// Implementors return the rendered text; *where* it goes (stdout, a file) is the caller's choice, so
/// reporters stay pure and unit-testable against their schema/consumer.
pub trait Reporter {
    /// Render the run into this reporter's format.
    fn render(&self, report: &RunReport) -> String;
}
