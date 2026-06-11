# CLI Reference

## Synopsis

```
riptide [OPTIONS] [PATHS]... [COMMAND]
```

## Commands

### `riptide [PATHS]` — Run Tests

The default command. Discovers and runs tests, applying impact analysis.

```bash
riptide tests/
riptide tests/ src/tests/
riptide tests/test_auth.py          # single file
riptide tests/test_auth.py::test_login  # single test (passed to pytest)
```

**Options:**

| Option | Short | Default | Description |
|---|---|---|---|
| `--workers N` | `-n N` | CPU count | Parallel worker threads |
| `--python BIN` | | `python3` | Python binary |
| `--coverage` | `-c` | off | Enable per-test coverage |
| `--all` | | off | Run all tests, skip impact analysis |
| `--isolate` | | off | Run one pytest process per test (legacy isolation). Default is batched — one pytest process per worker — which is much faster cold (see [ADR-009](../design/decisions.md)). `--coverage` always uses the isolated path to record a precise per-test dependency graph |
| `--pattern REGEX` | | `test_.*\.py\|.*_test\.py` | File discovery regex |
| `--db PATH` | | `.riptide.db` | SQLite state database path |
| `--timeout SECS` | | `300` | Per-test (or per-batch) wall-clock timeout in seconds; on expiry the process is killed and the affected test(s) recorded as an error |

Defaults can also be set in `[tool.riptide]` in `pyproject.toml`. Precedence is **explicit CLI flag > `pyproject.toml` value > built-in default** — see [Configuration](../guides/configuration.md).

**Exit codes:** See [Exit Codes](exit-codes.md).

---

### `riptide collect [PATHS]`

Discover and list all tests without running them. Useful for verifying discovery configuration.

```bash
riptide collect tests/
```

Output:
```
  ✓ 47 tests collected:
  tests/test_auth.py::test_login
  tests/test_auth.py::test_logout
  tests/test_auth.py::TestRegistration::test_valid_email
  ...
```

---

### `riptide clear`

Delete the state database, forcing a full re-run on the next invocation.

```bash
riptide clear
riptide clear --db custom.db
```

---

### `riptide coverage`

Print the coverage report from the last `--coverage` run without re-running tests.

```bash
riptide coverage
```

---

## Examples

```bash
# Standard development workflow
riptide tests/

# First run on a new project — run with --coverage to build the dep graph
# that unlocks precise source-level impact analysis
riptide tests/ --coverage --all

# CI run — 8 workers, coverage, explicit Python
riptide tests/ -n 8 --coverage --python .venv/bin/python

# Tighten the per-test timeout to 60 seconds
riptide tests/ --timeout 60

# Debug a specific test with full output
riptide tests/test_auth.py::test_login --all -n 1

# Force full re-run after major refactor
riptide clear && riptide tests/ --coverage --all
```
