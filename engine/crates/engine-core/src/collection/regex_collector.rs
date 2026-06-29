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
    ".riptide-spike-venv",
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

        for line in src.lines() {
            let trimmed = line.trim_start();
            if trimmed.is_empty() || trimmed.starts_with('#') {
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

            if is_class {
                let caps = self.class_re.captures(line).expect("class match");
                let cindent = caps.get(1).map_or(0, |m| m.as_str().len());
                let cname = caps.get(2).expect("class name").as_str().to_string();
                let bases = caps.get(3).map_or("", |m| m.as_str());
                let is_unittest = bases.contains("TestCase");
                // Collect from unittest subclasses (any name) and pytest `Test*` classes.
                class_ctx = if is_unittest || cname.starts_with("Test") {
                    Some((cname, cindent, is_unittest))
                } else {
                    None
                };
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
    }
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
