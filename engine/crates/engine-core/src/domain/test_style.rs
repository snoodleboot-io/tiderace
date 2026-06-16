use serde::{Deserialize, Serialize};

/// How a collected test is executed. Drives the per-style protocol (design doc 10) and the wire
/// `style` field the shim dispatches on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TestStyle {
    /// `def test_*` at module level.
    PytestFunction,
    /// A method on a (non-unittest) `Test*` class.
    PytestClassMethod,
    /// A method on a `unittest.TestCase` subclass (driven via stdlib `TestCase.run()`).
    UnittestMethod,
}

impl TestStyle {
    /// The wire token the Python shim dispatches on.
    pub fn wire(self) -> &'static str {
        match self {
            TestStyle::PytestFunction => "pytest_func",
            TestStyle::PytestClassMethod => "pytest_method",
            TestStyle::UnittestMethod => "unittest_method",
        }
    }
}
