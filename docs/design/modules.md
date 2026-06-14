# Module Design

## `main.rs` — Entry point & orchestration

Owns the CLI (`clap`) and wires the pipeline together. Subcommands: default run, `collect`,
`clear`, `coverage`, and `watch`. Resolves effective settings with precedence **CLI flag >
`pyproject.toml` > built-in default**, then drives collector → hasher → impact → runner →
reporter. Intentionally thin — the glue, not the logic.

---

## `config.rs` — Configuration

Parses `[tool.tiderace]` from `pyproject.toml`. Every key is optional; unknown keys are
rejected so typos surface. Keys: `workers`, `python`, `coverage`, `isolate`, `pattern`, `db`,
`paths`, `timeout`.

---

## `collector.rs` — Test discovery

Regex scan of Python sources — no interpreter needed. Recognises:

- top-level `def test_*` and `async def test_*`
- `class Test*` methods (pytest convention)
- `unittest.TestCase` subclasses **by base class**, regardless of name
- methods of non-test classes are correctly skipped

```rust
pub struct TestItem {
    pub test_id: String,        // pytest node id
    pub file_path: String,
    pub function_name: String,
    pub class_name: Option<String>,
}
```

Parametrized tests are collected as their base node id; pytest expands them at runtime and the
runner aggregates the `node_id[param]` cases back together.

---

## `hasher.rs` — File fingerprinting

- `hash_file(path)` → SHA-256 hex
- `hash_all_python_files(root)` → `HashMap<path, hash>` (leading `./` normalized so keys match
  the collector and coverage)
- `find_changed_files(current, db)` / `save_hashes(...)`

Skips `.git`, `__pycache__`, `.venv`, `venv`, `node_modules`.

---

## `db.rs` — Persistence

Wraps `rusqlite::Connection`; inline SQL, no ORM. `save_file_hash`/`get_file_hash`,
`save_test_result`/`get_last_result`, `save_test_deps`/`get_test_deps`,
`save_coverage`/`get_coverage`. All writes use `INSERT OR REPLACE`.

---

## `impact.rs` — Affected-test selection

```rust
pub struct ImpactAnalyzer<'a> {
    db: &'a Database,
    changed_files: Vec<String>,
    test_files: HashSet<String>,   // tells test-file vs source-file changes apart
}
```

A test runs if: its own file changed; it never ran before; a recorded dependency changed; or
it previously failed/errored. With **no** dependency graph, any *source* change re-runs it
conservatively. No changes at all → everything skips. ([ADR-007](decisions.md))

---

## `runner.rs` — Execution

```rust
pub struct Runner {
    pub workers: usize,
    pub python_bin: String,
    pub with_coverage: bool,
    pub coverage_dir: PathBuf,
    pub timeout: Duration,
    pub isolate: bool,
}
```

- **Batched** (default): one `pytest -rA` process per worker; per-test status parsed from the
  summary and aggregated across parametrized `node_id[param]` cases. Per-test timeout, `--`
  argument-injection guard, bounded output capture.
- **Isolated** (`--isolate`): one process per test (exit-code status).
- **Coverage**: batched under `coverage run` with `dynamic_context = test_function`; deps
  extracted from `coverage json --show-contexts` and matched by node-id suffix
  ([ADR-011](decisions.md)).

---

## `pool.rs` + `worker.py` — Warm worker pool

`pool.rs` manages long-lived `worker.py` processes (embedded in the binary) that import pytest
once and run node ids over newline-delimited JSON. Per-request timeout → kill + respawn;
crash detection via stdout EOF; a dedicated reader thread per worker avoids pipe deadlock.
`worker.py` evicts changed first-party modules from `sys.modules` before each run so a warm
re-run never executes stale code. Powers `tiderace watch`.

---

## `watcher.rs` — File watching

Debounced recursive watch via `notify` + `notify-debouncer-full` (rename-aware), yielding a
deduplicated batch of changed `.py` paths and ignoring artifact dirs and tiderace's own state.

---

## `reporter.rs` — Terminal output

`print_header`, `print_progress`, `print_summary`, coverage report. Uses `colored` (respects
`NO_COLOR`).
