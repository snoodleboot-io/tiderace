use serde::{Deserialize, Serialize};

/// How a collected test is executed. Drives the per-style protocol (design doc 10) and the wire
/// `style` field the shim dispatches on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TestStyle {
    /// `def test_*` at module level.
    Function,
    /// A method on a (non-unittest) `Test*` class.
    ClassMethod,
    /// A method on a `unittest.TestCase` subclass (driven via stdlib `TestCase.run()`).
    UnittestMethod,
    /// A class whose tests may be **inherited** from a base in another module (TID-26).
    ///
    /// Collection scans source text, so it cannot see through `class TestKuzuConformance(
    /// GraphStoreConformance)` to the methods it inherits — on a real corpus that silently dropped
    /// 129 tests, every backend conformance suite among them, while the run stayed green. The
    /// collector emits this marker for a class with a base it cannot rule out, and the shim (which
    /// has the live class) walks the MRO and reports one result per inherited method.
    InheritedMethods,
    /// A class the name rules do not recognise, whose bases this scan cannot see through (TID-26).
    ///
    /// `PackOverridesBuiltinTests(_SharedCatalogCase)` is a `unittest.TestCase` subclass
    /// *transitively*, which pytest collects regardless of its name — but the source text says
    /// neither "Test…" nor "TestCase". The shim decides against the real class and reports every
    /// test method it has, own and inherited, since the scan collected none of them.
    UnresolvedClass,
}

impl TestStyle {
    /// The wire token the Python shim dispatches on.
    pub fn wire(self) -> &'static str {
        match self {
            TestStyle::Function => "function",
            TestStyle::ClassMethod => "class_method",
            TestStyle::UnittestMethod => "unittest_method",
            TestStyle::InheritedMethods => "inherited_methods",
            TestStyle::UnresolvedClass => "unresolved_class",
        }
    }
}
