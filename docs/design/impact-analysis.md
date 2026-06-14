# Impact Analysis

Impact analysis is the core feature that makes tiderace fast on repeated runs. It answers the question: **given that these files changed, which tests need to re-run?**

## The Problem

A typical Python project has hundreds of tests. When you change one function in one file, naively you must run all tests to be safe. But most tests don't touch that file at all. Running them wastes time.

## How tiderace Solves It

tiderace builds and maintains a **dependency graph** mapping each test to the set of source files it executed during its last run. This graph comes from Python's `coverage.py` instrumentation.

```
test_login       → [src/auth.py, src/models.py, src/db.py]
test_register    → [src/auth.py, src/models.py, src/validators.py]
test_format_date → [src/utils.py]
test_send_email  → [src/email.py, src/utils.py, src/templates.py]
```

When `src/auth.py` changes:

- `test_login` → **run** (depends on auth.py)
- `test_register` → **run** (depends on auth.py)
- `test_format_date` → **skip** (no dependency on auth.py)
- `test_send_email` → **skip** (no dependency on auth.py)

## Decision Rules

For each test, tiderace applies these rules in order:

```
1. Has the test file itself changed?          → RUN
2. Has any file in the test's dep graph changed? → RUN
3. Did the test fail on its last run?            → RUN (always retry failures)
4. Is this the first run (no dep graph yet)?     → RUN
5. Otherwise                                     → SKIP
```

Rule 3 is important: tiderace never silently skips a previously failing test. You always know if something is broken.

## Building the Dep Graph

The dep graph is populated from coverage data. When you run with `--coverage`:

```bash
tiderace tests/ --coverage
```

tiderace runs each test as:

```bash
python -m coverage run --data-file=.tiderace-coverage/.coverage.<test_id> \
  --source=. --branch -m pytest <nodeid>
```

After the test completes, it extracts the file list from the coverage JSON:

```bash
python -m coverage json --data-file=... -o coverage.json
```

The `files` key lists every `.py` file that was imported or executed during that test. This list is stored in SQLite:

```sql
INSERT INTO test_file_deps (test_id, dep_path) VALUES (?, ?)
```

On subsequent runs, this data powers the skip decision without needing `--coverage` again.

## File Change Detection

tiderace uses SHA-256 hashing rather than file modification times or git status:

- **Reliable across filesystems** — mtime can be unreliable in Docker, network mounts, and CI
- **Content-based** — touching a file without changing it doesn't trigger re-runs
- **Git-independent** — works in any directory, not just git repos

On each run, tiderace hashes every `.py` file in the scanned paths and compares against hashes stored in `.tiderace.db`. Files whose hash has changed are marked as "changed" and used to compute affected tests.

After a run completes successfully, the new hashes are written back to the DB.

## Limitations

### Dynamic imports

tiderace cannot detect dependencies created by dynamic imports that coverage.py doesn't trace:

```python
# This IS detected (coverage traces the import)
import src.auth

# This might NOT be detected depending on execution path
module = importlib.import_module(f"src.{name}")
```

In practice this is rare, and the worst outcome is a missed dep that causes a false skip — which `--all` can always override.

### First run after `--coverage`

Impact analysis only becomes selective after at least one `--coverage` run has built the dep graph. On first run (or after `tiderace clear`), all tests execute.

### Monorepo / shared libraries

If a test imports a shared library outside the scanned path tree, that library's changes won't be detected. Configure `--paths` to include all relevant directories.
