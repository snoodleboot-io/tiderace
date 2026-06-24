# ADR: In-process backend — packaging (crate vs feature gate)

**Date:** 2026-06-24
**Status:** Proposed

> The **load-bearing** decision for this feature — *how tests are isolated under an embedded
> interpreter* — is already ratified in
> [ADR-E013](../../current/pure-rust-test-engine/design/adr/ADR-E013-inprocess-isolation.md):
> **fork-from-embedded** (not per-test reset / subinterpreters). This ticket-local ADR covers only the
> remaining packaging question.

## Context

`InProcessTransport` needs PyO3 / libpython linked. The engine workspace currently has **zero**
native-link dependencies and builds cleanly on Linux **and** the new Windows CI job. Pulling libpython
into `engine-core` directly risks both CI builds (libpython-dev availability, Windows linkage).

## Decision (proposed — confirm at implementation step 0)

Keep the in-process backend in a **separate, optional crate** (`engine-inproc`) that depends on
`engine-core` and implements `ShimTransport`, **off by default** and **excluded from the Windows job**.
The default engine (and Windows) keep the dependency-free `PipeTransport`/`SubprocessWorker`. Promote to
an `engine-core` feature flag only if the separate-crate boundary proves awkward.

## Alternatives considered

- **Feature flag in `engine-core`** (`--features inproc`) — simpler wiring, but puts an optional
  native-link dep in the core crate; easy to accidentally pull into the default build.
- **In `engine-core` unconditionally** — rejected: breaks the dependency-free core + Windows build.

## Rationale

The seam (`ShimTransport`) makes the transport pluggable from *outside* `engine-core`, so a separate
crate is natural and keeps libpython entirely off the default/Windows path — matching ADR-E008's
"capability-selected backend" model.

## Consequences

- ➕ Default + Windows builds stay dependency-free and green.
- ➕ libpython risk is quarantined to one opt-in crate.
- ➖ A little more wiring (capability detection selects the transport from the optional crate).
- Follow-up: the feasibility probe (DESIGN step 0) confirms libpython links in the workspace before any
  real implementation.
