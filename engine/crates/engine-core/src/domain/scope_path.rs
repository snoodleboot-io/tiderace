use serde::{Deserialize, Serialize};

/// Where a test sits in the module/class hierarchy — used for snapshot-layer locality (Phase 3+).
/// Phase 2 populates `module` (and `class` for class-based tests).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopePath {
    /// Dotted-or-slashed module identifier (e.g. `pkg/test_mod.py`).
    pub module: String,
    /// Enclosing class name for class-based tests, else `None`.
    pub class: Option<String>,
}

impl ScopePath {
    pub fn module(module: impl Into<String>) -> Self {
        Self {
            module: module.into(),
            class: None,
        }
    }

    pub fn with_class(module: impl Into<String>, class: impl Into<String>) -> Self {
        Self {
            module: module.into(),
            class: Some(class.into()),
        }
    }
}
