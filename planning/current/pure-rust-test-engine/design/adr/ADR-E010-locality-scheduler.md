# ADR-E010 — Duration-aware, scope-locality scheduler

**Status:** ✅ Accepted (design) · Depends on E003 (snapshots), E004/E006 (history).

## Context

Parallel execution across fork workers (separate processes, separate GILs) creates a tension:

- **Makespan:** to finish fastest, balance total work evenly across workers.
- **Fixture reuse:** module/class/session-scoped snapshots (E003) are per-worker; spreading
  tests of one module across many workers re-creates that scope's snapshot on each, wasting the
  expensive setup.

These goals conflict: pure load-balancing shreds fixture locality; pure locality can leave
workers idle.

## Decision

A `LocalityScheduler` that bin-packs with **both** objectives:

1. **Group** tests by their deepest shared snapshot scope (session → module → class) so a
   group can reuse one snapshot.
2. **Order** groups by historical duration, **longest-processing-time-first (LPT)**, to
   minimize makespan. Durations come from the timing history (SQLite, as today) and the cache
   (E004); on a cold first run, fall back to a heuristic (test count / static size).
3. **Assign** groups to workers greedily onto the least-loaded worker, but keep a group intact
   unless splitting clearly wins (group far larger than the average bin) — and even then, split
   along the *next* scope boundary so each shard still reuses a snapshot.

Scheduling is cheap Rust; it runs after collection + cache filtering, before fork.

## Consequences

- ➕ Good makespan **and** snapshot reuse — neither sacrificed by default.
- ➕ Uses data we already persist (timings) — old design's SQLite timing carries forward.
- ➖ Needs history to be optimal; cold runs are heuristic (acceptable, converges after one run).
- ➖ The split-vs-keep heuristic needs tuning; exposed as a config knob and validated by
   benchmarks.

## Alternatives considered

- **Round-robin / chunking (today's `runner.rs`):** simple, but ignores fixture locality and
  duration skew — rejected as default (kept as `RoundRobinScheduler` for debugging).
- **Pure LPT ignoring locality:** best raw makespan but re-runs expensive fixtures on every
  worker — rejected.
- **Pure locality ignoring duration:** wastes cores when one module dominates — rejected.

## Revisit trigger

If benchmarks show the locality penalty is negligible for most suites (cheap fixtures), simplify
toward pure LPT and treat locality as an opt-in for fixture-heavy projects.
