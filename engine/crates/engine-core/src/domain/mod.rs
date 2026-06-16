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
