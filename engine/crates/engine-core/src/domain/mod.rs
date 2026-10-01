//! The core domain vocabulary — the single source of truth other subsystems speak.
//! One public type per file (per project conventions).

mod node_id;
mod outcome;
mod run_report;
mod scope;
mod scope_path;
mod test_item;
mod test_result;
mod test_style;

pub use node_id::NodeId;
pub use outcome::Outcome;
pub use run_report::RunReport;
pub use scope::Scope;
pub use scope_path::ScopePath;
pub use test_item::TestItem;
pub use test_result::TestResult;
pub use test_style::TestStyle;

/// Deserialize a string field where the wire and old files spell "absent" as `""`.
pub(crate) fn empty_as_none<'de, D>(d: D) -> std::result::Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s: Option<String> = serde::Deserialize::deserialize(d)?;
    Ok(s.filter(|s| !s.is_empty()))
}
