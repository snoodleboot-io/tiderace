use anyhow::Result;
use rusqlite::{params, Connection};
use std::collections::HashMap;
use std::path::Path;

pub struct Database {
    conn: Connection,
}

impl Database {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        let db = Database { conn };
        db.init()?;
        Ok(db)
    }

    fn init(&self) -> Result<()> {
        self.conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS file_hashes (
                path TEXT PRIMARY KEY,
                hash TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS test_results (
                test_id TEXT PRIMARY KEY,
                file_path TEXT NOT NULL,
                status TEXT NOT NULL,
                duration_ms INTEGER NOT NULL,
                stdout TEXT,
                stderr TEXT,
                ran_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS test_file_deps (
                test_id TEXT NOT NULL,
                dep_path TEXT NOT NULL,
                PRIMARY KEY (test_id, dep_path)
            );

            CREATE TABLE IF NOT EXISTS coverage_data (
                run_id TEXT NOT NULL,
                file_path TEXT NOT NULL,
                lines_covered TEXT NOT NULL,
                lines_total INTEGER NOT NULL,
                ran_at TEXT NOT NULL,
                PRIMARY KEY (run_id, file_path)
            );
        ",
        )?;
        Ok(())
    }

    /// Store file hashes after a run
    pub fn save_file_hash(&self, path: &str, hash: &str) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT OR REPLACE INTO file_hashes (path, hash, updated_at) VALUES (?1, ?2, ?3)",
            params![path, hash, now],
        )?;
        Ok(())
    }

    /// Get stored hash for a file
    pub fn get_file_hash(&self, path: &str) -> Result<Option<String>> {
        let result = self.conn.query_row(
            "SELECT hash FROM file_hashes WHERE path = ?1",
            params![path],
            |row| row.get(0),
        );
        match result {
            Ok(h) => Ok(Some(h)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Record which source files a test depends on (from coverage data)
    pub fn save_test_deps(&self, test_id: &str, deps: &[String]) -> Result<()> {
        self.conn.execute(
            "DELETE FROM test_file_deps WHERE test_id = ?1",
            params![test_id],
        )?;
        for dep in deps {
            self.conn.execute(
                "INSERT OR IGNORE INTO test_file_deps (test_id, dep_path) VALUES (?1, ?2)",
                params![test_id, dep],
            )?;
        }
        Ok(())
    }

    /// Get all deps for a test
    pub fn get_test_deps(&self, test_id: &str) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT dep_path FROM test_file_deps WHERE test_id = ?1")?;
        let deps = stmt
            .query_map(params![test_id], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        Ok(deps)
    }

    /// Save test result
    pub fn save_test_result(&self, result: &crate::runner::TestResult) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT OR REPLACE INTO test_results 
             (test_id, file_path, status, duration_ms, stdout, stderr, ran_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                result.test_id,
                result.file_path,
                result.status.as_str(),
                result.duration_ms,
                result.stdout,
                result.stderr,
                now
            ],
        )?;
        Ok(())
    }

    /// Get last result for a test
    pub fn get_last_result(&self, test_id: &str) -> Result<Option<String>> {
        let result = self.conn.query_row(
            "SELECT status FROM test_results WHERE test_id = ?1",
            params![test_id],
            |row| row.get(0),
        );
        match result {
            Ok(s) => Ok(Some(s)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Persist a coverage report for a run. Stores executed and total line
    /// counts per file so coverage history is queryable after the run.
    pub fn save_coverage(
        &self,
        run_id: &str,
        coverage: &HashMap<String, crate::runner::CoverageInfo>,
    ) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        for (file, info) in coverage {
            self.conn.execute(
                "INSERT OR REPLACE INTO coverage_data
                 (run_id, file_path, lines_covered, lines_total, ran_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    run_id,
                    file,
                    info.executed_lines.to_string(),
                    info.total_lines,
                    now
                ],
            )?;
        }
        Ok(())
    }

    /// Read back persisted coverage rows for a run: `(file_path, executed, total)`.
    /// Read-side counterpart to [`save_coverage`]; consumed by tests and available
    /// for coverage-history tooling.
    #[allow(dead_code)]
    pub fn get_coverage(&self, run_id: &str) -> Result<Vec<(String, u32, u32)>> {
        let mut stmt = self.conn.prepare(
            "SELECT file_path, lines_covered, lines_total FROM coverage_data WHERE run_id = ?1",
        )?;
        let rows = stmt
            .query_map(params![run_id], |row| {
                let executed: String = row.get(1)?;
                let total: u32 = row.get(2)?;
                Ok((
                    row.get::<_, String>(0)?,
                    executed.parse().unwrap_or(0),
                    total,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::{CoverageInfo, TestResult, TestStatus};

    fn temp_db() -> (tempfile::TempDir, Database) {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(&dir.path().join("state.db")).unwrap();
        (dir, db)
    }

    #[test]
    fn file_hash_round_trip() {
        let (_d, db) = temp_db();
        assert!(db.get_file_hash("a.py").unwrap().is_none());
        db.save_file_hash("a.py", "deadbeef").unwrap();
        assert_eq!(
            db.get_file_hash("a.py").unwrap().as_deref(),
            Some("deadbeef")
        );
    }

    #[test]
    fn test_result_and_deps_round_trip() {
        let (_d, db) = temp_db();
        let res = TestResult {
            test_id: "t.py::test_x".into(),
            file_path: "t.py".into(),
            status: TestStatus::Failed,
            duration_ms: 12,
            stdout: Some("out".into()),
            stderr: None,
            covered_files: vec!["src/a.py".into(), "src/b.py".into()],
        };
        db.save_test_result(&res).unwrap();
        db.save_test_deps(&res.test_id, &res.covered_files).unwrap();
        assert_eq!(
            db.get_last_result("t.py::test_x").unwrap().as_deref(),
            Some("failed")
        );
        let mut deps = db.get_test_deps("t.py::test_x").unwrap();
        deps.sort();
        assert_eq!(deps, vec!["src/a.py".to_string(), "src/b.py".to_string()]);
    }

    #[test]
    fn deps_are_replaced_not_appended() {
        let (_d, db) = temp_db();
        db.save_test_deps("t", &["a".into(), "b".into()]).unwrap();
        db.save_test_deps("t", &["c".into()]).unwrap();
        assert_eq!(db.get_test_deps("t").unwrap(), vec!["c".to_string()]);
    }

    #[test]
    fn coverage_persists_and_reads_back() {
        // The W3 fix: coverage_data is actually written now.
        let (_d, db) = temp_db();
        let mut cov = HashMap::new();
        cov.insert(
            "src/a.py".to_string(),
            CoverageInfo {
                executed_lines: 8,
                total_lines: 10,
                percentage: 80.0,
            },
        );
        db.save_coverage("run1", &cov).unwrap();
        let rows = db.get_coverage("run1").unwrap();
        assert_eq!(rows, vec![("src/a.py".to_string(), 8, 10)]);
    }
}
