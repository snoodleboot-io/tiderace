# Decision Log

This document records key architectural decisions, the alternatives considered, and the rationale for each choice.

---

## ADR-001: Subprocess execution over PyO3 embedding

**Date:** 2024-01  
**Status:** Accepted

**Decision:** Run each test by spawning `python -m pytest <nodeid>` as a subprocess rather than embedding CPython via PyO3.

**Alternatives considered:**
- PyO3 embedding — direct CPython API calls from Rust
- Cython bridge — compile a thin C extension
- Custom Python interpreter via RustPython

**Rationale:**
- Full `conftest.py`, fixture, and plugin compatibility with zero reimplementation
- True process isolation — no shared import state between tests
- Simpler codebase; PyO3 binding lifecycle is complex
- Subprocess startup cost (~250ms) is offset by impact analysis savings

**Consequences:** Tests cannot be faster than ~250ms each due to interpreter startup. This is acceptable for the target use case (developer feedback loops, CI).

---

## ADR-002: SQLite for state persistence

**Date:** 2024-01  
**Status:** Accepted

**Decision:** Store file hashes, test results, and dep graphs in a local SQLite database.

**Alternatives considered:**
- JSON flat files — simpler but no ACID, harder to query
- Redis — fast but requires a daemon
- RON/TOML — human-readable but slow for large dep graphs
- Git notes — clever but git-dependent

**Rationale:**
- Zero infrastructure — one file, no daemon
- ACID guarantees matter when parallel workers write concurrently
- `rusqlite` with bundled feature compiles SQLite statically — no runtime dep
- Easy to inspect manually with `sqlite3` CLI

---

## ADR-003: SHA-256 for change detection over git status

**Date:** 2024-01  
**Status:** Accepted

**Decision:** Detect changed files by comparing SHA-256 hashes of file contents against stored values.

**Alternatives considered:**
- `git diff --name-only` — fast, but git-dependent and fragile in detached-HEAD CI
- File modification time — unreliable across Docker, NFS, and CI environments
- File size — too coarse; small changes share file sizes

**Rationale:**
- Works in any directory, with or without git
- Content-based — touching a file without editing it doesn't trigger re-runs
- SHA-256 is fast enough: 50MB of Python source hashes in <100ms

---

## ADR-004: Rayon for parallelism over async

**Date:** 2024-01  
**Status:** Accepted

**Decision:** Use Rayon thread pool for parallel test execution rather than async Tokio tasks.

**Alternatives considered:**
- `tokio::spawn` — async tasks
- `std::thread::spawn` — manual thread management
- `crossbeam` — channel-based work queue

**Rationale:**
- Tests are CPU-bound from subprocess wait perspective — threads fit better than async tasks
- Rayon's `par_iter()` is trivial to apply to a `Vec<TestItem>`
- No need for async I/O — `Command::output()` is a blocking call, which is correct here
- Rayon handles work-stealing automatically for uneven test durations

---

## ADR-005: Regex-based collection over Python AST

**Date:** 2024-01  
**Status:** Accepted

**Decision:** Discover test functions using regex scanning of Python source, not a Python AST parser.

**Alternatives considered:**
- `tree-sitter-python` Rust bindings — full AST
- `rustpython-parser` — Python parser in Rust
- Shell out to `python -m pytest --collect-only` — accurate but slow

**Rationale:**
- Regex covers 99% of real-world test patterns with zero dependencies
- `tree-sitter` adds ~2MB to binary size and build complexity for marginal gain
- `pytest --collect-only` takes 1-3s; regex scan takes <10ms on large codebases
- Accuracy gap is acceptable: missed tests simply fall through to pytest's own collection

---

## ADR-006: Trunk-based development with semantic versioning

**Date:** 2024-01  
**Status:** Accepted

**Decision:** Use trunk-based development (single `main` branch) with semantic versioning. Major version bumps are gated exclusively to the CI layer.

**Rationale:**
- Long-lived feature branches create merge debt; trunk-based forces integration discipline
- Semantic versioning communicates breaking changes clearly
- Gating major bumps to CI prevents accidental breaking releases from local developer machines
- Minor/patch versions are auto-computed from conventional commit messages in CI

See [Release Process](../guides/releases.md) for implementation details.

---

## ADR-007: Impact analysis is result-keyed; source-level precision requires coverage

**Date:** 2026-06  
**Status:** Accepted

**Decision:** A test is selected to run when (a) its own test file changed, (b) it
has no recorded prior result (never run), or (c) it previously failed or errored.
Beyond that, selection depends on the per-test dependency graph: when a coverage
graph exists, only tests whose recorded dependencies changed are re-run; when a
test has **no** graph, any change to a non-test (source) file conservatively
re-runs it. With no changes at all, every previously-run test is skipped.

**Context / problem:** The original implementation keyed the "already ran" check on
the presence of a dependency graph, which is only populated under `--coverage`.
As a result, a warm run **without** coverage re-ran the entire suite every time —
the headline impact-analysis feature was effectively a no-op without coverage.

**Rationale:**
- Keying step (b) on a prior *result* (not on deps) lets tiderace recognise a test
  as "already run" even when coverage was off, so unchanged warm runs skip — the
  expected behaviour.
- Source-to-test mapping is fundamentally impossible without a coverage graph, so
  the honest fallback is conservative (run on any source change) rather than
  unsafe (silently skip a test whose source changed).

**Consequences:** Precise source-level impact analysis requires one prior
`--coverage` run to build the graph. This is documented in the quickstart and
configuration guides. Verified by unit tests in `impact.rs` and end-to-end
integration tests in `tests/cli.rs`.

---

## ADR-008: Subprocess execution hardening (timeout, argument-injection guard, hashed data files)

**Date:** 2026-06  
**Status:** Accepted

**Decision:** Each test subprocess runs under a per-test wall-clock timeout
(default 300s, configurable via `--timeout` / `[tool.tiderace].timeout`); on expiry
the child is killed and recorded as an error. Node IDs and paths are passed after a
`--` end-of-options separator so a hostile path or filename cannot be parsed as a
pytest/coverage flag. Per-test coverage data files are named from a SHA-256 hash of
the test id rather than a lossy character substitution, and captured stdout/stderr
is bounded to 256 KiB before persistence.

**Context:** A security review of the runner flagged: argument injection via
attacker-controlled node IDs/paths flowing into `Command` (a checked-out hostile
repo is a realistic CI threat); no timeout (a hanging test pins a worker forever);
a non-injective `safe_id` transform that let distinct tests collide on the same
coverage file under parallel execution; and unbounded in-memory output capture.

**Rationale:** These close real correctness and resource-exhaustion issues without
changing the subprocess-per-test model (ADR-001). Output is captured via temp files
rather than pipes specifically to avoid the pipe-buffer deadlock that a timeout plus
piped stdio would otherwise hit. The parallel result collection was also moved off a
shared `Mutex<Vec<_>>` (and its lock-poisoning `unwrap`s) to `par_iter().map().collect()`.

**Consequences:** Adds a `wait-timeout` dependency. Behaviour is covered by unit
tests for status mapping, hashing, and output capping.

---

## ADR-009: Evolve the execution layer (batch → persistent workers → embedded subinterpreters)

**Date:** 2026-06  
**Status:** Accepted (Stages A and B implemented; C planned)

### Stage B as shipped — `tiderace watch`

A `WorkerPool` (`tiderace/pool.rs`) spawns N long-lived Python workers (`tiderace/worker.py`,
embedded in the binary) that `import pytest` once and run node ids fed as
newline-delimited JSON. `tiderace watch` (`notify` + `notify-debouncer-full`) re-runs only
impact-selected tests on each save against the warm pool, so cycles after the first pay no
pytest import. A security review drove the must-fix robustness that landed:
- **Per-request timeout → kill + respawn** (a hung test never wedges the pool; the run never hangs).
- **Crash detection** via stdout EOF / channel disconnect → respawn; the test is recorded Error.
- **Correct staleness handling**: workers evict *all first-party modules* on any change (keeping
  pytest/deps warm), call `importlib.invalidate_caches()`, set `dont_write_bytecode`, and a
  `conftest.py` change recycles the whole pool — verified by an edit-then-rerun correctness test.
- **Framing safety**: requests/responses are serde/`json.dumps`-encoded only (never hand-built),
  with an adversarial test proving a node id containing `\n`/`\r`/NUL cannot forge a frame.
- **No pipe deadlock**: a dedicated reader thread per worker drains stdout into a channel;
  at most one request is in flight per worker.

Hardening follow-ups (all now landed): **process-group kill** — spawned test processes run in
their own group and timeouts/crashes signal the whole group, so a test's grandchildren are
reaped too (`procutil.rs`); **per-worker recycle** after `MAX_WORKER_REQUESTS` to bound
long-session memory/fd growth; **incremental hashing** in the watch loop (each cycle hashes
only the paths the watcher reported, not the whole tree); and **coverage-context precise
impact** ([ADR-011](#adr-011-per-test-impact-via-coverage-dynamic-contexts-precise-and-batched)).
Warm workers remain a trusted-local-dev convenience — CI / untrusted code should use the
isolated single-shot path.

---

## ADR-010: Stage C (embedded CPython subinterpreters) — rejected

**Date:** 2026-06  
**Status:** Rejected (do not revisit until the C-extension ecosystem is multi-interpreter-ready)

**Decision:** Do **not** pursue ADR-009 stage C (embed CPython via PyO3 and run per-core
PEP 684 own-GIL subinterpreters). The subprocess-based worker pool (stage B) remains the
execution architecture.

**What was tested (feasibility spike, Python 3.12.3, throwaway crate — tiderace untouched):**
1. **Embedding works.** PyO3 0.23 builds and runs pytest in-process here (after a no-sudo
   `libpython3.12.so` symlink, since dev headers were absent).
2. **PEP 684 parallelism is real.** Four own-GIL subinterpreters ran 4× CPU-bound work in
   **1.20×** the single-interpreter time (vs **4.04×** for plain GIL-bound threads) — genuine
   in-process parallelism.
3. **Real workloads crash the process.** A subinterpreter imports pure-Python pytest fine,
   but the moment it touches a single-phase-init C extension it **segfaults the whole
   process**. Reproduced with the **stdlib** `_decimal` module:
   `mpd_setminalloc ... a second time` → `munmap_chunk(): invalid pointer` → core dump.

**Rationale (why rejected):**
- Isolated subinterpreters (`check_multi_interp_extensions=1`) reject/corrupt single-phase C
  extensions, which is still nearly the entire ecosystem (numpy, pandas, pydantic-core, lxml,
  many sqlalchemy/db drivers) and even parts of the **stdlib**. tiderace's core value is full
  pytest/ecosystem compatibility; stage C trades that away.
- The failure mode is *worse* than subprocesses: one incompatible `import` in one test takes
  down **all** subinterpreters sharing the process, whereas the stage-B pool gives OS-level
  isolation — a crashing test only loses (and respawns) its own worker.
- Per-unit overhead is also high (one subinterpreter ran the same work ~2.2× slower than a
  plain thread), so even the parallelism win is partially eroded.

**Consequences:** The subprocess worker pool (stage B) is the practical performance ceiling.
Revisit stage C only when multi-phase init (`Py_mod_multiple_interpreters`) is widespread
across the C-extension ecosystem — years out. Further perf/precision work should go into the
existing CPython-subprocess model: coverage-context impact (ADR-011 below), incremental
hashing, and the stage-B robustness follow-ups.

---

## ADR-011: Per-test impact via coverage dynamic contexts (precise *and* batched)

**Date:** 2026-06  
**Status:** Accepted

**Decision:** Build the per-test dependency graph from **coverage dynamic contexts** recorded
during a single *batched* `coverage run`, instead of a separate `coverage run` per test.

**Context:** Previously `--coverage` forced the isolated one-process-per-test path purely to
attribute coverage to individual tests — precise but ~4–5× slower than batched. coverage.py's
`dynamic_context = test_function` tags every measured line with the running test, so one
batched run over many tests still yields per-test attribution.

**How:** Each batch runs `coverage run --rcfile=<generated> --data-file=<per-batch> -m pytest …`
with `dynamic_context = test_function`. After the run, `coverage combine` + `coverage json
--show-contexts` gives, per file, which contexts touched it. Context names carry a
package-dependent prefix (e.g. `pkg.tests.test_x.test_a`), so tests are matched on the stable
**suffix** `{file_stem}.{func}` / `{file_stem}.{Class}.{method}` (last 2–3 dotted components)
rather than a predicted full name. Suffix collisions (same-stem files) only *over*-include
deps, which is safe (a test may re-run unnecessarily, never wrongly skip).

**Consequences:** `--coverage` is now batched: measured ~4.5× faster (49-test fixture: 5.6s vs
25.2s isolated) while producing a precise graph — editing one source module re-runs only its
dependent tests. `--isolate` still forces the one-process-per-test path. A latent path
normalization bug surfaced and was fixed: the file hasher now strips a leading `./` so change
detection keys match the collector's and coverage's relative paths. Verified by unit tests
(suffix mapping) and an integration test (edit-one-module → exactly its test re-runs).

**Decision:** Stop paying CPython + pytest startup *per test*. Keep pytest as the
execution engine (full fixture/plugin/assertion-rewrite compatibility) but change
*how* tiderace drives it, in three stages:

- **A — Batched subprocess (this change).** Instead of one `pytest <nodeid>` process
  per test, distribute the selected tests across the worker pool and run **one pytest
  process per worker** (`pytest <nodeid> <nodeid> …`). Per-test outcomes are recovered
  by parsing pytest's `-rA` summary lines (which contain the exact node id). Collapses
  *N* interpreter startups into *W* (= worker count). The precise per-test coverage
  path (one process per test) is retained **only** for `--coverage` runs, which build
  the dependency graph occasionally; everyday non-coverage runs use the batched path.
- **B — Persistent warm workers (planned).** Long-lived CPython worker processes with
  pytest pre-imported, fed node ids over IPC (the model `pytest-xdist` uses via
  execnet). Startup is paid once per worker for the life of a daemon/watch session,
  not once per run.
- **C — Embedded CPython subinterpreters (planned, longer-term).** Embed libpython via
  PyO3 and run per-core subinterpreters (PEP 684 per-interpreter GIL, Python 3.12+) for
  true in-process parallelism with zero per-test startup. Gated on PyO3 subinterpreter
  support maturing.

**Context / problem:** ADR-001 chose one subprocess per test for isolation, accepting
~250 ms startup per test. Benchmarks showed this makes the cold full run ~4–5× slower
than in-process pytest (which starts up once for the whole suite) — the cost is the
*process count*, not pytest. The per-test isolation that justified ADR-001 is rarely
needed: pytest already isolates tests within one process.

**This supersedes** the per-test-process aspect of ADR-001. The subprocess-vs-PyO3
rationale of ADR-001 still holds for stages A and B (CPython, full compatibility);
stage C revisits embedding deliberately and with subinterpreters, not naive PyO3.

**Consequences:**
- Batched runs report per-test status via `-rA` parsing and drop `-x` (run the whole
  batch, report every result). Per-test wall-clock timing is coarser in batch mode
  (timeout applies per batch). Precise per-test coverage still requires a `--coverage`
  run, which uses the isolated path.
- An `--isolate` escape hatch forces the legacy one-process-per-test behaviour when a
  suite genuinely needs interpreter isolation.
- Expected: cold full run drops from ~Nx pytest toward pytest-parity (stage A) and
  below it with warm workers / subinterpreters (B, C).
