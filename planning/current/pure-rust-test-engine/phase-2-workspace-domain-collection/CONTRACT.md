# Phase 2 — Frozen Contract (consumed by Phases 3–7)

> Status: ✅ Frozen as of Phase 2 completion (2026-06-17). Implemented in `engine/crates/engine-core`.
> Changes to anything here require pausing dependent lanes and re-presenting (per PIPELINE §9).

## 1. Domain types (`engine_core::domain`, one type per file)

| Type | Shape (public surface) |
|------|------------------------|
| `NodeId` | newtype `String`; `new`, `as_str`, `file()` (before first `::`), `segments()` (after). `Eq+Hash+Ord+Serialize`. Format: `relpath.py::Class::method` or `relpath.py::func`, path **relative to the collection root**. |
| `TestStyle` | enum `PytestFunction` \| `PytestClassMethod` \| `UnittestMethod`; `wire()` → `"pytest_func"` \| `"pytest_method"` \| `"unittest_method"`. |
| `Scope` | enum `Function<Class<Module<Package<Session`; `rank()`, `outlives()`. Derived `Ord` matches `rank`. |
| `ScopePath` | `{ module: String, class: Option<String> }`; `module()`, `with_class()`. |
| `Outcome` | closed enum `Passed\|Failed\|Skipped\|XFail\|XPass\|Error`; `is_failure()` (= `Failed\|Error`; XPass strictness deferred to Phase 4), `from_wire()`. |
| `TestResult` | `{ node_id: NodeId, outcome: Outcome, duration_ms: u64, detail: String }`. |
| `RunReport` | `{ results: Vec<TestResult> }`; `total()`, `tally(Outcome)`, `exit_code()` (0/1). |
| `TestItem` | `{ node_id: NodeId, style: TestStyle, scope_path: ScopePath }`. |

> **Not yet present** (added by later phases, will extend — not break — these): `Fixture`,
> `FixtureRequest`, `Mark`, `Parametrization`/`ParamSet`, `RichDiff`/`SubexprValue`/`ValueRepr`,
> `InputClosure`/`CacheKey`. Phase 4 adds `RichDiff` to `TestResult` (canonical shape per
> [design/02](../design/02-domain-model.md)); Phase 5 adds `InputClosure`.

## 2. Seams (`engine_core`)

- `collection::Collector` — `fn collect(&self, root: &Path) -> Result<Vec<TestItem>>`. Default impl
  `RegexCollector` (no Python import). Node ids are **relative to `root`**.
- `exec::Worker` — `fn run(&mut self, items: &[TestItem]) -> Result<Vec<TestResult>>`. Default impl
  `ForkWorker`; the no-fork `SubprocessWorker` (ADR-E008) and future workers slot in here.
- `error::EngineError` — `Collection | Exec | Io` (thiserror). An *engine* failure; a *test* error
  is `Outcome::Error` on a `TestResult`, never an `EngineError`.

## 3. Wire protocol (`engine_core::exec::shim_protocol` ↔ `engine/py-shim/shim.py`)

- **Framing:** length-prefixed — `u32` little-endian byte length, then a UTF-8 JSON payload.
  (bincode/msgpack remains the deferred [ADR-E002](../design/adr/ADR-E002-execution-substrate.md)
  option; JSON is the frozen Phase-2 choice.)
- **Startup:** shim → `{"ready": true, "pid": <int>}`.
- **Request** (`ExecRequest`): `{"node_id": str, "style": <wire token>, "deadline_ms": int}`.
- **Response** (`ExecResponse`): `{"node_id": str, "outcome": <wire token>, "detail": str}`.
- **Outcome wire tokens:** `passed|failed|skipped|xfail|xpass|error` (unknown → `Error`).
- **Substrate shape:** the Wellspring (`python shim.py <root>`) imports `<root>` once; each request
  `os.fork()`s a pristine child; child→parent result over an `os.pipe`; deadline/crash →
  `outcome:"error"`. Forward fields for later phases (fixture args, post-fork setup, coverage
  deltas) attach to `ExecRequest`/`ExecEvent` without changing the framing.

## 4. Layout / invariants

- Workspace at `engine/` (`engine-core` lib + `engine-cli` bin `riptide`); isolated from the legacy
  root `tiderace` crate until it is retired.
- One public type per file, snake_case filename; `Result`/`?` + `thiserror`, no panics in lib code.
- Native thread pools pinned (`OPENBLAS/OMP/MKL_NUM_THREADS=1`) when launching the Wellspring —
  Phase 3 generalizes this as a thread/`reinit_after_fork` policy.
