use anyhow::Result;
use regex::Regex;
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

#[derive(Debug, Clone)]
pub struct TestItem {
    /// Unique ID: "path/to/test_file.py::test_function_name"
    pub test_id: String,
    pub file_path: String,
    pub function_name: String,
    pub class_name: Option<String>,
}

impl TestItem {
    pub fn pytest_nodeid(&self) -> String {
        match &self.class_name {
            Some(cls) => format!("{}::{}::{}", self.file_path, cls, self.function_name),
            None => format!("{}::{}", self.file_path, self.function_name),
        }
    }
}

/// Discover all test items in the given paths
pub fn collect_tests(paths: &[PathBuf], pattern: &str) -> Result<Vec<TestItem>> {
    let mut items = Vec::new();
    let file_re = Regex::new(pattern)?;

    for path in paths {
        if path.is_file() {
            if path.extension().is_some_and(|e| e == "py") {
                collect_from_file(path, &mut items)?;
            }
        } else {
            for entry in WalkDir::new(path)
                .follow_links(true)
                .into_iter()
                .filter_map(|e| e.ok())
                .filter(|e| {
                    let p = e.path();
                    p.is_file()
                        && p.extension().is_some_and(|ext| ext == "py")
                        && file_re.is_match(&p.file_name().unwrap_or_default().to_string_lossy())
                        && !p.components().any(|c| {
                            let s = c.as_os_str().to_string_lossy();
                            s == ".git" || s == "__pycache__" || s == ".venv" || s == "venv"
                        })
                })
            {
                collect_from_file(entry.path(), &mut items)?;
            }
        }
    }

    Ok(items)
}

/// Parse a Python file and extract test functions (fast regex-based, no AST).
///
/// Recognises two kinds of test classes so unittest-style suites collect correctly:
///   * pytest convention — `class TestFoo:` (name starts with `Test`)
///   * unittest convention — any class deriving from `TestCase` /
///     `unittest.TestCase`, regardless of its name (e.g. `class AuthCase(unittest.TestCase)`)
///
/// A method is attributed to the nearest enclosing test class only while its
/// indentation stays deeper than that class's `def`/`class` column; once a line
/// returns to the class's indent (or shallower) the class scope is closed. This
/// avoids the previous bug where a module-level `def test_*` after a class, or a
/// second class, was mis-attributed.
fn collect_from_file(path: &Path, items: &mut Vec<TestItem>) -> Result<()> {
    let content = fs::read_to_string(path)?;
    let file_path = path.to_string_lossy().to_string();
    parse_source(&content, &file_path, items)
}

/// Parse already-loaded Python source into test items. Split out from
/// [`collect_from_file`] so the parsing logic is unit-testable without I/O.
fn parse_source(content: &str, file_path: &str, items: &mut Vec<TestItem>) -> Result<()> {
    // Capture indentation, class name, and the (optional) base-class list.
    let class_re = Regex::new(r"^(\s*)class\s+(\w+)\s*(?:\(([^)]*)\))?\s*:")?;
    // Capture indentation and the test function name (pytest `test*` convention).
    let func_re = Regex::new(r"^(\s*)(?:async\s+)?def\s+(test\w*)\s*\(")?;

    // Nearest enclosing class scope: (name, indent_of_class_keyword, is_test_class).
    let mut enclosing: Option<(String, usize, bool)> = None;

    for line in content.lines() {
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let indent = line.len() - trimmed.len();

        // Leaving the class body: dedent to or past the class column on a
        // non-class line closes the scope.
        if let Some((_, class_indent, _)) = &enclosing {
            if indent <= *class_indent && !class_re.is_match(line) {
                enclosing = None;
            }
        }

        if let Some(caps) = class_re.captures(line) {
            let class_indent = caps[1].len();
            let class_name = caps[2].to_string();
            let bases = caps.get(3).map_or("", |m| m.as_str());
            let is_test = is_test_class(&class_name, bases);
            enclosing = Some((class_name, class_indent, is_test));
            continue;
        }

        if let Some(caps) = func_re.captures(line) {
            let func_name = caps[2].to_string();

            // A `def` nested inside a class belongs to that class; a method of a
            // non-test class is not a test and is skipped entirely.
            let class_name = match &enclosing {
                Some((name, class_indent, is_test)) if indent > *class_indent => {
                    if !is_test {
                        continue;
                    }
                    Some(name.clone())
                }
                _ => None,
            };

            let test_id = match &class_name {
                Some(cls) => format!("{}::{}::{}", file_path, cls, func_name),
                None => format!("{}::{}", file_path, func_name),
            };

            items.push(TestItem {
                test_id,
                file_path: file_path.to_string(),
                function_name: func_name,
                class_name,
            });
        }
    }

    Ok(())
}

/// Decide whether a class is a test class. True when the name follows the pytest
/// `Test*` convention, or any base class looks like a unittest `TestCase`.
fn is_test_class(name: &str, bases: &str) -> bool {
    if name.starts_with("Test") {
        return true;
    }
    bases.split(',').any(|b| {
        let b = b.trim();
        b == "TestCase" || b.ends_with(".TestCase") || b.ends_with("TestCase")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(src: &str) -> Vec<String> {
        let mut items = Vec::new();
        parse_source(src, "t.py", &mut items).unwrap();
        items.into_iter().map(|i| i.test_id).collect()
    }

    #[test]
    fn collects_top_level_functions() {
        let ids = ids("def test_a():\n    pass\n\ndef test_b():\n    pass\n");
        assert_eq!(ids, vec!["t.py::test_a", "t.py::test_b"]);
    }

    #[test]
    fn ignores_non_test_functions() {
        let ids = ids("def helper():\n    pass\n\ndef test_real():\n    pass\n");
        assert_eq!(ids, vec!["t.py::test_real"]);
    }

    #[test]
    fn collects_async_test_functions() {
        let ids = ids("async def test_a():\n    pass\n\ndef test_b():\n    pass\n");
        assert_eq!(ids, vec!["t.py::test_a", "t.py::test_b"]);
    }

    #[test]
    fn collects_async_methods_in_test_class() {
        let src = "class TestX:\n    async def test_m(self):\n        pass\n";
        assert_eq!(ids(src), vec!["t.py::TestX::test_m"]);
    }

    #[test]
    fn collects_pytest_test_class_methods() {
        let src = "class TestThing:\n    def test_one(self):\n        pass\n\n    def test_two(self):\n        pass\n";
        assert_eq!(
            ids(src),
            vec!["t.py::TestThing::test_one", "t.py::TestThing::test_two"]
        );
    }

    #[test]
    fn collects_unittest_testcase_subclass_regardless_of_name() {
        // The W4 fix: a unittest class NOT named Test* still collects, WITH its class prefix.
        let src = "import unittest\n\nclass AuthCase(unittest.TestCase):\n    def test_login(self):\n        pass\n";
        assert_eq!(ids(src), vec!["t.py::AuthCase::test_login"]);
    }

    #[test]
    fn collects_bare_testcase_base() {
        let src = "class Flow(TestCase):\n    def test_x(self):\n        pass\n";
        assert_eq!(ids(src), vec!["t.py::Flow::test_x"]);
    }

    #[test]
    fn skips_methods_of_non_test_classes() {
        // A test_* method inside a plain class is NOT a pytest test — must be skipped,
        // not emitted as a bogus top-level node id.
        let src = "class Helper:\n    def test_not_really(self):\n        pass\n";
        assert!(ids(src).is_empty());
    }

    #[test]
    fn module_level_function_after_class_is_not_attributed() {
        // Regression for the old indentation bug.
        let src =
            "class TestA:\n    def test_in(self):\n        pass\n\ndef test_top():\n    pass\n";
        assert_eq!(ids(src), vec!["t.py::TestA::test_in", "t.py::test_top"]);
    }

    #[test]
    fn handles_two_adjacent_classes() {
        let src = "class TestA:\n    def test_a(self):\n        pass\n\nclass TestB:\n    def test_b(self):\n        pass\n";
        assert_eq!(ids(src), vec!["t.py::TestA::test_a", "t.py::TestB::test_b"]);
    }

    #[test]
    fn decorators_and_comments_do_not_break_attribution() {
        let src = "class TestD:\n    # a comment\n    @pytest.mark.slow\n    def test_dec(self):\n        pass\n";
        assert_eq!(ids(src), vec!["t.py::TestD::test_dec"]);
    }

    #[test]
    fn is_test_class_rules() {
        assert!(is_test_class("TestFoo", ""));
        assert!(is_test_class("Foo", "unittest.TestCase"));
        assert!(is_test_class("Foo", "Base, TestCase"));
        assert!(!is_test_class("Foo", "object"));
        assert!(!is_test_class("Helper", ""));
    }
}
