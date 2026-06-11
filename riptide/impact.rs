use anyhow::Result;
use std::collections::HashSet;

use crate::collector::TestItem;
use crate::db::Database;

/// Decides which collected tests actually need to run, given the set of files
/// that changed since the last run and the persisted per-test dependency graph.
pub struct ImpactAnalyzer<'a> {
    db: &'a Database,
    changed_files: Vec<String>,
    /// Paths of all known test files, used to tell test-file changes apart from
    /// source-file changes when a test has no recorded dependency graph.
    test_files: HashSet<String>,
}

impl<'a> ImpactAnalyzer<'a> {
    pub fn new(db: &'a Database, changed_files: Vec<String>, tests: &[TestItem]) -> Self {
        let test_files = tests.iter().map(|t| t.file_path.clone()).collect();
        ImpactAnalyzer {
            db,
            changed_files,
            test_files,
        }
    }

    /// Partition tests into `(to_run, skipped)`.
    pub fn filter_affected(&self, tests: &[TestItem]) -> Result<(Vec<TestItem>, Vec<TestItem>)> {
        let changed: HashSet<&String> = self.changed_files.iter().collect();
        // Did any *non-test* (i.e. source) file change? When a test lacks a
        // dependency graph we cannot map source edits to it, so any source
        // change forces it to run conservatively.
        let source_changed = self
            .changed_files
            .iter()
            .any(|f| !self.test_files.contains(f));

        let mut to_run = Vec::new();
        let mut skipped = Vec::new();
        for test in tests {
            if self.should_run(test, &changed, source_changed)? {
                to_run.push(test.clone());
            } else {
                skipped.push(test.clone());
            }
        }
        Ok((to_run, skipped))
    }

    fn should_run(
        &self,
        test: &TestItem,
        changed: &HashSet<&String>,
        source_changed: bool,
    ) -> Result<bool> {
        // 1. The test's own file changed — always run.
        if changed.contains(&test.file_path) {
            return Ok(true);
        }

        // 2. Never run before (no recorded result) — must run to establish a baseline.
        //    NOTE: this is keyed on a prior *result*, not on deps, so a test that
        //    ran once without coverage is correctly recognised as "already run"
        //    and can be skipped on an unchanged re-run (the warm-run fix).
        let last = self.db.get_last_result(&test.test_id)?;
        if last.is_none() {
            return Ok(true);
        }

        // 3. Decide on the basis of the dependency graph.
        let deps = self.db.get_test_deps(&test.test_id)?;
        if deps.is_empty() {
            // No dep graph (coverage was never run for this test): we can't map
            // source edits to it, so re-run whenever any source file changed.
            if source_changed {
                return Ok(true);
            }
        } else if deps.iter().any(|d| changed.contains(d)) {
            // A known dependency changed.
            return Ok(true);
        }

        // 4. Always re-run a test that previously failed or errored.
        if matches!(last.as_deref(), Some("failed") | Some("error")) {
            return Ok(true);
        }

        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::{TestResult, TestStatus};

    fn item(file: &str, func: &str) -> TestItem {
        TestItem {
            test_id: format!("{}::{}", file, func),
            file_path: file.to_string(),
            function_name: func.to_string(),
            class_name: None,
        }
    }

    fn record(db: &Database, test: &TestItem, status: TestStatus, deps: &[&str]) {
        let res = TestResult {
            test_id: test.test_id.clone(),
            file_path: test.file_path.clone(),
            status,
            duration_ms: 1,
            stdout: None,
            stderr: None,
            covered_files: deps.iter().map(|s| s.to_string()).collect(),
        };
        db.save_test_result(&res).unwrap();
        if !deps.is_empty() {
            db.save_test_deps(&test.test_id, &res.covered_files)
                .unwrap();
        }
    }

    fn temp_db() -> (tempfile::TempDir, Database) {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(&dir.path().join("state.db")).unwrap();
        (dir, db)
    }

    #[test]
    fn never_run_test_runs() {
        let (_d, db) = temp_db();
        let t = item("tests/test_a.py", "test_x");
        let a = ImpactAnalyzer::new(&db, vec![], std::slice::from_ref(&t));
        let (run, skip) = a.filter_affected(std::slice::from_ref(&t)).unwrap();
        assert_eq!(run.len(), 1);
        assert_eq!(skip.len(), 0);
    }

    #[test]
    fn unchanged_test_with_no_changes_is_skipped() {
        // The W1 warm-run fix: a previously-passed test with no file changes skips,
        // even with no coverage dependency graph.
        let (_d, db) = temp_db();
        let t = item("tests/test_a.py", "test_x");
        record(&db, &t, TestStatus::Passed, &[]);
        let a = ImpactAnalyzer::new(&db, vec![], std::slice::from_ref(&t));
        let (run, skip) = a.filter_affected(std::slice::from_ref(&t)).unwrap();
        assert_eq!(run.len(), 0);
        assert_eq!(skip.len(), 1);
    }

    #[test]
    fn test_reruns_when_its_own_file_changes() {
        let (_d, db) = temp_db();
        let t = item("tests/test_a.py", "test_x");
        record(&db, &t, TestStatus::Passed, &["src/foo.py"]);
        let a = ImpactAnalyzer::new(
            &db,
            vec!["tests/test_a.py".into()],
            std::slice::from_ref(&t),
        );
        let (run, _) = a.filter_affected(std::slice::from_ref(&t)).unwrap();
        assert_eq!(run.len(), 1);
    }

    #[test]
    fn test_reruns_when_a_dependency_changes() {
        let (_d, db) = temp_db();
        let t = item("tests/test_a.py", "test_x");
        record(&db, &t, TestStatus::Passed, &["src/foo.py"]);
        let a = ImpactAnalyzer::new(&db, vec!["src/foo.py".into()], std::slice::from_ref(&t));
        let (run, _) = a.filter_affected(std::slice::from_ref(&t)).unwrap();
        assert_eq!(run.len(), 1);
    }

    #[test]
    fn test_with_deps_skips_when_unrelated_file_changes() {
        let (_d, db) = temp_db();
        let t = item("tests/test_a.py", "test_x");
        record(&db, &t, TestStatus::Passed, &["src/foo.py"]);
        let a = ImpactAnalyzer::new(&db, vec!["src/bar.py".into()], std::slice::from_ref(&t));
        let (run, skip) = a.filter_affected(std::slice::from_ref(&t)).unwrap();
        assert_eq!(run.len(), 0);
        assert_eq!(skip.len(), 1);
    }

    #[test]
    fn test_without_deps_reruns_on_any_source_change() {
        // Conservative: no dep graph + a source file changed => must run.
        let (_d, db) = temp_db();
        let t = item("tests/test_a.py", "test_x");
        record(&db, &t, TestStatus::Passed, &[]);
        let a = ImpactAnalyzer::new(&db, vec!["src/foo.py".into()], std::slice::from_ref(&t));
        let (run, _) = a.filter_affected(std::slice::from_ref(&t)).unwrap();
        assert_eq!(run.len(), 1);
    }

    #[test]
    fn previously_failed_test_always_reruns() {
        let (_d, db) = temp_db();
        let t = item("tests/test_a.py", "test_x");
        record(&db, &t, TestStatus::Failed, &["src/foo.py"]);
        let a = ImpactAnalyzer::new(&db, vec![], std::slice::from_ref(&t));
        let (run, _) = a.filter_affected(std::slice::from_ref(&t)).unwrap();
        assert_eq!(run.len(), 1);
    }
}
