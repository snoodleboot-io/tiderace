# Database Schema

The `.tiderace.db` SQLite database can be queried directly for debugging or integration purposes.

## Tables

See [State Database](../design/database.md) for the full schema with column descriptions.

## Useful Queries

```sql
-- Tests that ran in the last run
SELECT test_id, status, duration_ms
FROM test_results
ORDER BY ran_at DESC;

-- All failing tests
SELECT test_id, ran_at
FROM test_results
WHERE status IN ('failed', 'error');

-- Slowest tests
SELECT test_id, duration_ms
FROM test_results
ORDER BY duration_ms DESC
LIMIT 20;

-- Which tests depend on a given file
SELECT test_id
FROM test_file_deps
WHERE dep_path LIKE '%auth%';

-- How many tests each source file affects
SELECT dep_path, COUNT(*) as test_count
FROM test_file_deps
GROUP BY dep_path
ORDER BY test_count DESC;
```
