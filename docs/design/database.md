# State Database

tiderace persists state in a local SQLite database (`.tiderace.db` by default). This file is machine-local and should not be committed to version control.

## Schema

### `file_hashes`

Stores the last-seen SHA-256 hash of every Python source file.

```sql
CREATE TABLE file_hashes (
    path        TEXT PRIMARY KEY,
    hash        TEXT NOT NULL,
    updated_at  TEXT NOT NULL      -- ISO 8601 timestamp
);
```

Used to detect which files changed since the last run.

### `test_results`

Stores the outcome of every test execution.

```sql
CREATE TABLE test_results (
    test_id     TEXT PRIMARY KEY,  -- e.g. "tests/test_auth.py::test_login"
    file_path   TEXT NOT NULL,     -- e.g. "tests/test_auth.py"
    status      TEXT NOT NULL,     -- "passed" | "failed" | "error" | "skipped"
    duration_ms INTEGER NOT NULL,
    stdout      TEXT,
    stderr      TEXT,
    ran_at      TEXT NOT NULL
);
```

Used to always re-run previously failing tests.

### `test_file_deps`

Maps each test to the source files it executed (built from coverage data).

```sql
CREATE TABLE test_file_deps (
    test_id   TEXT NOT NULL,
    dep_path  TEXT NOT NULL,
    PRIMARY KEY (test_id, dep_path)
);
```

This is the core of impact analysis. When `dep_path` changes, `test_id` must re-run.

### `coverage_data`

Stores per-run coverage summaries for reporting.

```sql
CREATE TABLE coverage_data (
    run_id        TEXT NOT NULL,
    file_path     TEXT NOT NULL,
    lines_covered TEXT NOT NULL,  -- JSON array of line numbers
    lines_total   INTEGER NOT NULL,
    ran_at        TEXT NOT NULL,
    PRIMARY KEY (run_id, file_path)
);
```

## Operations

### Resetting State

```bash
tiderace clear
# or manually:
rm .tiderace.db
```

After clearing, the next run will execute all tests and rebuild the state from scratch.

### Inspecting State

Since it's a standard SQLite database, you can inspect it directly:

```bash
sqlite3 .tiderace.db

# See all test results
SELECT test_id, status, duration_ms FROM test_results ORDER BY ran_at DESC;

# See which tests depend on a file
SELECT test_id FROM test_file_deps WHERE dep_path LIKE '%auth%';

# See files that changed most recently
SELECT path, updated_at FROM file_hashes ORDER BY updated_at DESC LIMIT 10;
```

## Sharing State

The DB is intentionally machine-local. Each developer's machine maintains its own state reflecting their local working tree. CI runners also maintain their own DB (typically per-job or cached across runs).

!!! tip "Caching in CI"
    Cache `.tiderace.db` between CI runs keyed on the branch name for maximum impact analysis benefit. See the [CI/CD guide](../guides/releases.md) for the GitHub Actions cache configuration.
