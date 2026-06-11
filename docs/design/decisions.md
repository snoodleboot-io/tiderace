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

See [Release Process](releases.md) for implementation details.

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
- Keying step (b) on a prior *result* (not on deps) lets riptide recognise a test
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
(default 300s, configurable via `--timeout` / `[tool.riptide].timeout`); on expiry
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
