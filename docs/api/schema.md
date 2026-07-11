# On-Disk State & Wire Format

tiderace has **no database** (no SQLite, no `.tiderace.db`). Warm state is a single JSON file, and the
Rust↔Python seam is a length-prefixed JSON frame. Both are documented here, derived directly from the
code.

## `.riptide-state.json` — impact-skip state

Written at `<root>/.riptide-state.json` by impact-aware `riptide-daemon run` (`engine-daemon/src/persist.rs`).
It records each test's last outcome and dependency footprint plus the content hash of every touched
file, so a later run re-executes only the tests whose dependencies changed. A missing or unparseable
file is treated as a cold start (empty state).

### `PersistedState`

| Field | Type | Meaning |
|---|---|---|
| `files` | map: `string → string` | relative source path → content hash (hex) at the time it was last run |
| `tests` | map: `string → TestRecord` | node id → its last result and dependency footprint |

### `TestRecord`

| Field | Type | Meaning |
|---|---|---|
| `outcome` | `string` | last wire outcome (e.g. `passed`, `failed`, `error`, `skipped`) |
| `detail` | `string` | failure/error detail (empty on pass) |
| `deps` | `string[]` | relative source files this test touched (from coverage) |

Both maps are sorted (`BTreeMap`), so the file is stable across writes.

### Example

```json
{
  "files": {
    "src/auth.py": "9f2a1c…",
    "src/util.py": "0b7e44…"
  },
  "tests": {
    "tests/test_auth.py::test_login": {
      "outcome": "passed",
      "detail": "",
      "deps": ["src/auth.py", "src/util.py"]
    },
    "tests/test_auth.py::test_logout": {
      "outcome": "passed",
      "detail": "",
      "deps": ["src/auth.py"]
    }
  }
}
```

On re-run, `changed_files()` diffs current hashes against `files`; `plan()` runs any test that is new
or whose `deps` intersect the changed set, and serves the rest from cache.

---

## ShimTransport wire frame

The engine and the Python shim exchange exactly one request/response pair per test over the
`ShimTransport` seam (`engine-core/src/exec/shim_protocol.rs`). Each message is a **length-prefixed
JSON frame**: a 4-byte little-endian `u32` payload length, followed by that many bytes of JSON.
Fields that are absent/default are skipped on the wire, so a fixtureless, coverage-off exchange is
byte-identical to the minimal frame.

### `ExecRequest` (engine → shim)

| Field | Type | On wire when | Meaning |
|---|---|---|---|
| `node_id` | `string` | always | the test to run |
| `style` | `string` | always | style token: `pytest_func`, `pytest_method`, or `unittest_method` |
| `deadline_ms` | `u64` | always | per-test deadline; a child that exceeds it is killed and reported `error` |
| `post_fork` | `FixtureInstance[]` | non-empty | Function-scope fixtures to set up in the forked child (topo order) |
| `reinit` | `string[]` | non-empty | fixture node ids to rebuild after fork (fork-fragile resources) |
| `fixture_args` | object | non-empty | assembled argument map the test body is invoked with |
| `force_no_fork` | `bool` | `true` | ask the shim to run in-process (no fork) where sound; it still forks non-restorable modules |

### `ExecResponse` (shim → engine)

| Field | Type | On wire when | Meaning |
|---|---|---|---|
| `node_id` | `string` | always | echoes the test id |
| `outcome` | `string` | always | wire outcome token (`passed` / `failed` / `error` / `skipped` / `xfail` / `xpass`) |
| `detail` | `string` | default `""` | failure/error message |
| `coverage` | map: `string → u32[]` | default `{}` | per-test touched source: relative path → sorted line numbers (populated only under coverage) |

### Example exchange

Request:

```json
{ "node_id": "tests/test_auth.py::test_login", "style": "pytest_func",
  "deadline_ms": 5000, "force_no_fork": true }
```

Response:

```json
{ "node_id": "tests/test_auth.py::test_login", "outcome": "passed",
  "coverage": { "src/auth.py": [10, 11, 14], "src/util.py": [3] } }
```
