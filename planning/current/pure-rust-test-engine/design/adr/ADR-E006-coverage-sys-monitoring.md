# ADR-E006 — Coverage via `sys.monitoring` (PEP 669), settrace fallback

**Status:** ✅ Accepted (design)

## Context

The content-addressed cache (E004) and impact analysis (11) both need each test's
**executed-source closure** — precisely which source lines/files a test actually touched. The
classic mechanism, `coverage.py` via `sys.settrace`, imposes a 2–5× runtime tax, which is
unacceptable for an *always-on* dependency tracker.

CPython 3.12 introduced **PEP 669 `sys.monitoring`**: low-overhead, tool-scoped instrumentation
with per-event callbacks and the ability to disable events per-location once seen.

## Decision

Capture per-test coverage through a `CoverageCollector` trait with two implementations:

- **`MonitoringCollector`** (default, CPython **3.12+**) — uses `sys.monitoring` line/branch
  events, registered under our tool id, scoped to the test body, emitting the touched-file set
  back through the shim.
- **`TraceCollector`** (fallback, CPython **≤3.11**) — `sys.settrace`-based, accepted as slower
  for older interpreters.

Coverage runs **inside the fork worker** for the single test it executes, so the closure is
exactly that test's footprint. The resulting `DepGraph` feeds both the cache key builder and the
impact analyzer.

## Consequences

- ➕ Cheap enough to leave on by default on 3.12+, which is what makes precise caching/impact
   viable rather than an opt-in `--coverage` mode (a limitation of the old design).
- ➕ Per-test granularity is natural because each test is its own forked process.
- ➖ Two implementations to maintain; the ≤3.11 path is materially slower (documented).
- ➖ `sys.monitoring` semantics differ across 3.12/3.13/3.14 point releases → pin behavior with
   a conformance test per supported minor version.

## Alternatives considered

- **`coverage.py` (settrace/ctracer):** heavy, slow, an external dependency we'd be wrapping —
  rejected as the default (its model also informed old ADR-011).
- **Static import-graph analysis only:** misses dynamic imports, `getattr`, plugin-loaded code,
  and conditional branches → unsound closures → unsafe cache — rejected as the sole source
  (may augment as a fast pre-filter).
- **eBPF/ptrace line tracking:** powerful but Linux-specific and complex — parked.

## Revisit trigger

If `sys.monitoring` overhead is still too high under fork (per-fork registration cost), explore
registering the tool once in the wellspring and inheriting across fork, or sampling strategies.
