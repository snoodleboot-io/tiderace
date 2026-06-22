//! Reporters (Phase 7, design 13) — render a finished [`RunReport`](crate::domain::RunReport) into
//! the formats CI and humans consume, behind one [`Reporter`] seam (ADR-E005).
//!
//! Shipped: [`TerminalReporter`] (default human output), [`JunitXmlReporter`] (the CI lingua franca),
//! [`JsonReporter`] (machine-readable). GitHub annotations + SARIF are the same pattern (a `render`
//! impl) and are the remaining formats. One type per file.

mod github_reporter;
mod json_reporter;
mod junit_xml_reporter;
#[allow(clippy::module_inception)]
// file name = snake_case of the `Reporter` trait (project convention)
mod reporter;
mod sarif_reporter;
mod terminal_reporter;

pub use github_reporter::GithubReporter;
pub use json_reporter::JsonReporter;
pub use junit_xml_reporter::JunitXmlReporter;
pub use reporter::Reporter;
pub use sarif_reporter::SarifReporter;
pub use terminal_reporter::TerminalReporter;
