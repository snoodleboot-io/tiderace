# Stage B — Persistent worker pool: IPC contract & lane map

Goal: `tiderace watch` holds a pool of W long-lived Python workers that import pytest
**once**. Each edit→test cycle dispatches only the affected node ids to warm workers,
so cycles after the first pay ~zero interpreter/pytest startup.

## IPC protocol (the frozen interface every lane builds against)

Transport: **newline-delimited JSON**, one message per line.
- Rust → worker on the worker's **stdin**.
- worker → Rust on the worker's **stdout** (worker logs/errors go to **stderr**).

### Handshake (once, at worker startup)
Worker, after `import pytest`, emits exactly:
```json
{"ready": true, "pid": 12345}
```
Rust waits for this before dispatching (confirms pytest imported successfully).

### Request (Rust → worker), one per line
```json
{"nodeid": "tests/test_x.py::test_a", "invalidate": ["tests/test_x.py", "src/y.py"]}
```
- `nodeid`: a single pytest node id to run.
- `invalidate`: file paths whose modules must be evicted from `sys.modules` before the
  run (changed files since the worker last imported them). May be empty.

### Response (worker → Rust), one per line, per request
```json
{"nodeid": "tests/test_x.py::test_a", "status": "passed", "duration_ms": 12, "summary": null}
```
- `status` ∈ `passed | failed | skipped | error`.
- `duration_ms`: integer wall-clock of the run() call.
- `summary`: optional short failure reason (string) or null.

### Shutdown
Rust closes the worker's stdin (EOF) → worker breaks its loop and exits 0.

## Worker semantics (Lane P)
1. `import pytest` once at startup; emit the ready handshake.
2. Loop over stdin lines. For each request:
   - For each path in `invalidate`, drop the matching module(s) from `sys.modules`
     (so pytest re-imports changed test/source files — required for assertion
     rewriting and fresh code).
   - Run the single node id via `pytest.main([...])` with a tiny in-process plugin that
     captures the `call`-phase report outcome (passed/failed/skipped/error).
   - Emit the response line; flush stdout.
3. On EOF, exit.

## Lane map (dependencies)

```
CONTRACT (this file, frozen) ──▶ ┌─ Lane P  Python worker (tiderace/worker.py)        [subagent]
                                 ├─ Lane R  Rust pool + watch loop (tiderace/pool.rs)  [spine — owned by orchestrator]
                                 ├─ Lane Wt notify-based debounced file watcher helper [subagent]
                                 ├─ Lane Bn benchmark: warm re-run latency scenario     [subagent]
                                 └─ Lane Sx security/robustness review of the pool/IPC  [subagent, read-only]
                                          │
                                 Aggregation: integrate → cargo test (unit+integration) →
                                 clippy -Dwarnings → fmt → coverage ≥80 → bench warm re-run
```

Genuinely parallel (disjoint files / read-only): P, Wt, Bn, Sx. The Rust pool +
`watch` command (R) is the coupled spine and is built serially by the orchestrator,
then integrates the others.

## Acceptance
- `tiderace watch` runs an initial pass, then re-runs only impacted tests on file save,
  with 2nd-cycle latency ≪ a cold run (target: warm re-run of a handful of tests in
  well under the cold full-run time; pool import paid once).
- All existing tests stay green; new unit tests for protocol (de)serialization and the
  worker (a smoke test driving worker.py directly); clippy -Dwarnings; coverage ≥80.
- Worker crash is handled (pool restarts a dead worker; run does not hang).
