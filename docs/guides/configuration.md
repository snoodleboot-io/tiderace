# Configuration

riptide is configured via command-line flags and optionally a `pyproject.toml` section.

## CLI Flags

| Flag | Default | Description |
|---|---|---|
| `[PATHS]` | `tests/ test/` | Test directories or files to scan |
| `-n, --workers` | `0` (CPU count) | Parallel worker threads |
| `--python` | `python3` | Python binary to use |
| `-c, --coverage` | off | Enable per-test coverage |
| `--all` | off | Ignore impact analysis, run everything |
| `--pattern` | `test_.*\.py\|.*_test\.py` | Regex for test file discovery |
| `--db` | `.riptide.db` | Path to SQLite state database |
| `--timeout` | `300` | Per-test wall-clock timeout in seconds |

A test that exceeds `--timeout` is killed and recorded as an error.

## pyproject.toml

riptide reads defaults from a `[tool.riptide]` section in your `pyproject.toml`:

```toml
[tool.riptide]
workers = 8                          # int
python = ".venv/bin/python"          # string
coverage = true                      # bool
pattern = "test_.*\\.py"             # string (regex)
db = ".riptide.db"                   # path
paths = ["tests/", "integration/"]   # list of strings
timeout = 300                        # int (seconds)
```

| Key | Type | Description |
|---|---|---|
| `workers` | int | Parallel worker threads |
| `python` | string | Python binary to use |
| `coverage` | bool | Enable per-test coverage |
| `pattern` | string (regex) | Test file discovery regex |
| `db` | path | SQLite state database path |
| `paths` | list of strings | Default test directories or files to scan |
| `timeout` | int (seconds) | Per-test wall-clock timeout |

### Precedence

Configuration is resolved in this order, highest priority first:

**explicit CLI flag > `pyproject.toml` value > built-in default**

So a flag passed on the command line always wins; if a setting is not given on the CLI, the `[tool.riptide]` value is used; otherwise the built-in default applies.

## Environment Variables

| Variable | Description |
|---|---|
| `RIPTIDE_WORKERS` | Override worker count |
| `RIPTIDE_DB` | Override DB path |
| `RIPTIDE_PYTHON` | Override Python binary |

## Test Discovery

riptide finds tests by:

1. Walking directories matching `--pattern` for file names
2. Scanning each file for `def test_*` functions (top-level and inside `class Test*`)
3. Building node IDs in pytest format: `path/to/test_file.py::TestClass::test_name`

!!! warning "Fixtures and conftest.py"
    riptide delegates execution to `pytest` as a subprocess, so all fixtures, `conftest.py`, parametrize, and marks work as normal. riptide controls *which* tests run and *how many at once* — not how they execute.

## Recommended .gitignore

```gitignore
.riptide.db
.riptide-coverage/
```

The state database is machine-local. Each developer and each CI runner maintains their own.
