# CLI Reference

## Synopsis

```
tiderace [OPTIONS] [PATHS]... [COMMAND]
```

## Commands

### `tiderace [PATHS]` — Run Tests

The default command. Discovers and runs tests, applying impact analysis.

```bash
tiderace tests/
tiderace tests/ src/tests/
tiderace tests/test_auth.py          # single file
tiderace tests/test_auth.py::test_login  # single test (passed to pytest)
```

**Options:**

| Option | Short | Default | Description |
|---|---|---|---|
| `--workers N` | `-n N` | CPU count | Parallel worker threads |
| `--python BIN` | | `python3` | Python binary |
| `--coverage` | `-c` | off | Enable per-test coverage |
| `--all` | | off | Run all tests, skip impact analysis |
| `--isolate` | | off | Run one pytest process per test (legacy isolation). Default is batched — one pytest process per worker — which is much faster cold (see [ADR-009](../design/decisions.md)). `--coverage` is also batched and stays precise via coverage dynamic contexts ([ADR-011](../design/decisions.md)) |
| `--pattern REGEX` | | `test_.*\.py\|.*_test\.py` | File discovery regex |
| `--db PATH` | | `.tiderace.db` | SQLite state database path |
| `--timeout SECS` | | `300` | Per-test (or per-batch) wall-clock timeout in seconds; on expiry the process is killed and the affected test(s) recorded as an error |

Defaults can also be set in `[tool.tiderace]` in `pyproject.toml`. Precedence is **explicit CLI flag > `pyproject.toml` value > built-in default** — see [Configuration](../guides/configuration.md).

**Exit codes:** See [Exit Codes](exit-codes.md).

---

### `tiderace collect [PATHS]`

Discover and list all tests without running them. Useful for verifying discovery configuration.

```bash
tiderace collect tests/
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

### `tiderace clear`

Delete the state database, forcing a full re-run on the next invocation.

```bash
tiderace clear
tiderace clear --db custom.db
```

---

### `tiderace coverage`

Print the coverage report from the last `--coverage` run without re-running tests.

```bash
tiderace coverage
```

---

### `tiderace watch [PATHS]`

Start a long-lived watcher backed by a **warm pool** of Python workers that import pytest
once. After an initial run, tiderace re-runs only the impact-selected tests on each file
save — and because the workers stay warm, cycles after the first pay no interpreter/pytest
startup (sub-second feedback loops). Press Ctrl-C to stop.

```bash
tiderace watch tests/
tiderace watch tests/ -n 8 --python .venv/bin/python
```

```
  ✓ collected 47 tests
  ⚡ warm pool ready: 8 workers
  ✓ 47 passed · 0 failed · 0 skipped · 0.9s

  👀 watching for changes — Ctrl-C to stop

  ⚡ 1 file(s) changed → 6 test(s)
  ✓ 6 passed · 0 failed · 0 skipped · 0.18s
```

Changed files are evicted from the workers' module cache before each cycle, so a warm
re-run always reflects the code on disk; a `conftest.py` change recycles the pool. Warm
workers are a convenience for trusted local development — use the isolated single-shot
path (`--isolate`, or `--coverage`) for CI or untrusted code.

---

## Examples

```bash
# Standard development workflow
tiderace tests/

# First run on a new project — run with --coverage to build the dep graph
# that unlocks precise source-level impact analysis
tiderace tests/ --coverage --all

# CI run — 8 workers, coverage, explicit Python
tiderace tests/ -n 8 --coverage --python .venv/bin/python

# Tighten the per-test timeout to 60 seconds
tiderace tests/ --timeout 60

# Debug a specific test with full output
tiderace tests/test_auth.py::test_login --all -n 1

# Force full re-run after major refactor
tiderace clear && tiderace tests/ --coverage --all
```
