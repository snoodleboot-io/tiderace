use std::fs;
use std::path::Path;

use regex::Regex;

use crate::collection::Collector;
use crate::domain::{NodeId, ScopePath, TestItem, TestStyle};
use crate::error::Result;

/// Directory names never descended into during collection.
const SKIP_DIRS: &[&str] = &[
    "__pycache__",
    ".git",
    ".venv",
    "venv",
    ".tiderace-spike-venv",
    ".tiderace-bench-venv",
    ".pytest_cache",
    "node_modules",
];

/// Regex-based collector — evolves `tiderace/collector.rs`. Recognizes module-level `def test_*`
/// (pytest functions), methods of `Test*` classes (pytest class methods), and methods of
/// `unittest.TestCase` subclasses (driven later via stdlib `TestCase.run()`). Indentation tracks
/// class scope; no Python import is performed.
pub struct RegexCollector {
    class_re: Regex,
    func_re: Regex,
}

impl Default for RegexCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl RegexCollector {
    pub fn new() -> Self {
        Self {
            class_re: Regex::new(r"^(\s*)class\s+(\w+)\s*(?:\(([^)]*)\))?\s*:")
                .expect("valid class regex"),
            func_re: Regex::new(r"^(\s*)(?:async\s+)?def\s+(test\w*)\s*\(")
                .expect("valid func regex"),
        }
    }

    fn is_test_file(name: &str) -> bool {
        name.ends_with(".py") && (name.starts_with("test_") || name.ends_with("_test.py"))
    }

    fn walk(&self, dir: &Path, root: &Path, out: &mut Vec<TestItem>) -> Result<()> {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if path.is_dir() {
                if !SKIP_DIRS.contains(&name.as_str()) {
                    self.walk(&path, root, out)?;
                }
            } else if Self::is_test_file(&name) {
                let rel = path.strip_prefix(root).unwrap_or(&path);
                let rel_str = rel.to_string_lossy().replace('\\', "/");
                let src = fs::read_to_string(&path)?;
                self.scan_source(&rel_str, &src, out);
            }
        }
        Ok(())
    }

    /// Scan one file's source into test items. Separated from I/O so it is unit-testable.
    fn scan_source(&self, rel: &str, src: &str, out: &mut Vec<TestItem>) {
        // Open class context: (name, indent_len, is_unittest).
        let mut class_ctx: Option<(String, usize, bool)> = None;
        // A class the name rules do NOT recognise but which has a base this scan cannot see through
        // (TID-26). `PackOverridesBuiltinTests(_SharedCatalogCase)` is a `unittest.TestCase`
        // subclass *transitively*, which pytest collects regardless of its name — the text says
        // neither "Test…" nor "TestCase". Held until the class closes so a marker is only emitted
        // for one that actually contains tests: (name, indent, saw_test_method).
        let mut unresolved: Option<(String, usize, bool)> = None;
        // The open triple-quoted string, if any. Suites embed Python source in string literals to
        // feed `ast.parse`, and a `class P:` at column 0 inside one read as a real class — closing
        // the enclosing test class and silently dropping every test after it (TID-26).
        let mut in_string: Option<&str> = None;

        for line in src.lines() {
            // Inside a triple-quoted string nothing is code; look only for its close.
            if let Some(delim) = in_string {
                if line.contains(delim) {
                    in_string = None;
                }
                continue;
            }
            let trimmed = line.trim_start();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            // An odd number of triple quotes opens one that spans the following lines; an even
            // number (`x = """one line"""`) opens and closes here and changes nothing.
            for delim in [r#"""""#, "'''"] {
                if line.matches(delim).count() % 2 == 1 {
                    in_string = Some(delim);
                    break;
                }
            }
            if in_string.is_some() {
                continue;
            }
            let indent = line.len() - trimmed.len();
            let is_class = self.class_re.is_match(line);
            let is_func = self.func_re.is_match(line);

            // A new construct at or left of the class column closes the class scope.
            if let Some((_, cindent, _)) = &class_ctx {
                if (is_class || is_func) && indent <= *cindent {
                    class_ctx = None;
                }
            }
            if let Some((uname, uindent, saw_test)) = &unresolved {
                if (is_class || is_func) && indent <= *uindent {
                    if *saw_test {
                        out.push(TestItem::new(
                            NodeId::new(format!("{rel}::{uname}")),
                            TestStyle::UnresolvedClass,
                            ScopePath::with_class(rel, uname.clone()),
                        ));
                    }
                    unresolved = None;
                }
            }
            // Inside an unrecognised class, note whether it has tests but collect nothing: the shim
            // reports its methods, because it is the only side that can tell whether the class is a
            // test class at all.
            if let Some((_, uindent, saw_test)) = &mut unresolved {
                if indent > *uindent {
                    if is_func {
                        *saw_test = true;
                    }
                    continue;
                }
            }

            if is_class {
                // A class nested INSIDE the open test class — a local exception, a stub built in a
                // test body — must not close it. The indent check above already cleared the context
                // for any class at or left of the class column, so a context still open here means
                // this one is deeper. Overwriting it dropped every remaining test in the enclosing
                // class: one `class ReadTimeout(Exception):` inside a method silently cost 44 tests
                // on a real corpus (TID-26).
                if class_ctx.is_some() {
                    continue;
                }
                let caps = self.class_re.captures(line).expect("class match");
                let cindent = caps.get(1).map_or(0, |m| m.as_str().len());
                let cname = caps.get(2).expect("class name").as_str().to_string();
                let bases = caps.get(3).map_or("", |m| m.as_str());
                let is_unittest = bases.contains("TestCase");
                // Collect from unittest subclasses (any name) and pytest `Test*` classes.
                if is_unittest || cname.starts_with("Test") {
                    // TID-26: a base this scan cannot rule out may carry inherited tests that never
                    // appear in this file. Mark the class so the shim, which has the live object,
                    // can walk its MRO. Emitted per class, not per method, so collection stays a
                    // source scan and the body's own methods are still collected individually.
                    if has_opaque_base(bases) {
                        out.push(TestItem::new(
                            NodeId::new(format!("{rel}::{cname}")),
                            TestStyle::InheritedMethods,
                            ScopePath::with_class(rel, cname.clone()),
                        ));
                    }
                    class_ctx = Some((cname, cindent, is_unittest));
                } else if has_opaque_base(bases) {
                    // Not recognised by name, and its bases are opaque — it may still be a test
                    // class by inheritance. Defer to the shim rather than guess from the text.
                    unresolved = Some((cname, cindent, false));
                } else {
                    class_ctx = None;
                }
                continue;
            }

            if is_func {
                let caps = self.func_re.captures(line).expect("func match");
                let fname = caps.get(2).expect("func name").as_str().to_string();
                match &class_ctx {
                    Some((cname, cindent, is_unittest)) if indent > *cindent => {
                        let style = if *is_unittest {
                            TestStyle::UnittestMethod
                        } else {
                            TestStyle::ClassMethod
                        };
                        out.push(TestItem::new(
                            NodeId::new(format!("{rel}::{cname}::{fname}")),
                            style,
                            ScopePath::with_class(rel, cname.clone()),
                        ));
                    }
                    _ if indent == 0 => {
                        out.push(TestItem::new(
                            NodeId::new(format!("{rel}::{fname}")),
                            TestStyle::Function,
                            ScopePath::module(rel),
                        ));
                    }
                    _ => {}
                }
            }
        }
        // The file can end while still inside an unrecognised class.
        if let Some((uname, _, true)) = unresolved {
            out.push(TestItem::new(
                NodeId::new(format!("{rel}::{uname}")),
                TestStyle::UnresolvedClass,
                ScopePath::with_class(rel, uname.clone()),
            ));
        }
    }
}

/// Whether a class's base list contains anything that could contribute inherited tests (TID-26).
///
/// The known-inert bases are the ones that provably carry no `test_*` of their own: `object` and the
/// `unittest` case classes every `TestCase` subclass already names. Anything else — a shared
/// conformance suite, a mixin, a re-exported alias — is opaque to a source scan, so the class is
/// marked and the shim resolves it against the real MRO.
///
/// The bias is deliberate: a false positive costs one cheap round trip that returns nothing, while a
/// false negative silently drops every test in the class, which is the bug being fixed.
fn has_opaque_base(bases: &str) -> bool {
    const INERT: &[&str] = &[
        "object",
        "TestCase",
        "unittest.TestCase",
        "IsolatedAsyncioTestCase",
        "unittest.IsolatedAsyncioTestCase",
        "AsyncioTestCase",
    ];
    bases
        .split(',')
        .map(str::trim)
        .filter(|b| !b.is_empty())
        // Keyword arguments in a base list (`metaclass=...`) are not bases.
        .filter(|b| !b.contains('='))
        .any(|b| !INERT.contains(&b))
}

impl Collector for RegexCollector {
    fn collect(&self, root: &Path) -> Result<Vec<TestItem>> {
        let mut out = Vec::new();
        self.walk(root, root, &mut out)?;
        out.sort_by(|a, b| a.node_id.cmp(&b.node_id));
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect_source(src: &str) -> Vec<TestItem> {
        let mut out = Vec::new();
        RegexCollector::new().scan_source("test_mod.py", src, &mut out);
        out
    }

    #[test]
    fn finds_module_level_function() {
        let items = collect_source("def test_x():\n    assert True\n");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].node_id.as_str(), "test_mod.py::test_x");
        assert_eq!(items[0].style, TestStyle::Function);
    }

    #[test]
    fn finds_async_function() {
        let items = collect_source("async def test_async():\n    pass\n");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].style, TestStyle::Function);
    }

    #[test]
    fn pytest_class_methods_get_class_in_node_id() {
        let src = "class TestThing:\n    def test_a(self):\n        pass\n    def test_b(self):\n        pass\n";
        let items = collect_source(src);
        let ids: Vec<_> = items.iter().map(|i| i.node_id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "test_mod.py::TestThing::test_a",
                "test_mod.py::TestThing::test_b"
            ]
        );
        assert!(items.iter().all(|i| i.style == TestStyle::ClassMethod));
    }

    #[test]
    fn unittest_case_detected_by_base_regardless_of_name() {
        let src = "import unittest\nclass ArithmeticCase(unittest.TestCase):\n    def test_m(self):\n        self.assertTrue(True)\n";
        let items = collect_source(src);
        assert_eq!(items.len(), 1);
        assert_eq!(
            items[0].node_id.as_str(),
            "test_mod.py::ArithmeticCase::test_m"
        );
        assert_eq!(items[0].style, TestStyle::UnittestMethod);
    }

    #[test]
    fn module_function_after_class_is_not_attributed_to_class() {
        let src =
            "class TestThing:\n    def test_a(self):\n        pass\n\ndef test_top():\n    pass\n";
        let items = collect_source(src);
        let ids: Vec<_> = items.iter().map(|i| i.node_id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["test_mod.py::TestThing::test_a", "test_mod.py::test_top"]
        );
    }

    #[test]
    fn non_test_class_and_non_test_def_are_ignored() {
        let src = "class Helper:\n    def test_looks_like(self):\n        pass\n\ndef helper():\n    pass\n";
        // Helper is neither a unittest subclass nor Test*-named, so its method is skipped;
        // `helper` is not a `test_*` function.
        assert!(collect_source(src).is_empty());
    }
}
