# ADR-E005 — Cargo workspace + trait-based DI seams

**Status:** ✅ Accepted (design)

## Context

Today the project is a single `tiderace` binary. The new design has **two** front-ends sharing
one engine (CLI + daemon), needs to be unit-testable at its seams, and must stay open for
extension (new worker kinds, cache tiers, reporters) without modifying call sites — i.e. SOLID's
OCP + DIP at the architectural level.

## Decision

Restructure into a **Cargo workspace**:

```text
crates/
├── engine-core/   # pure library: no front-end I/O concerns
├── engine-cli/    # thin CLI over engine-core
├── engine-daemon/ # JSON-RPC test server over engine-core
└── py-shim/       # Rust-shipped Python substrate shim
```

`engine-core` exposes its variation points as **traits** (DIP seams):

| Trait | Default impl | Why a seam |
|---|---|---|
| `Worker` | `ForkWorker` | platform fallback, free-threaded + remote futures |
| `Cache` | `TieredCache(Local, Remote)` | CI sharing; `NullCache` for debugging |
| `Collector` | `RegexCollector` | swap to `AstCollector` for precision |
| `Scheduler` | `LocalityScheduler` | tune makespan vs fixture reuse |
| `CoverageCollector` | `MonitoringCollector` | `TraceCollector` on ≤3.11 |
| `Reporter` | `TerminalReporter` | JSON/JUnit/GitHub/SARIF |

Code style: **one class/type per file**, snake_case filenames matching the type
(`fork_worker.rs` → `ForkWorker`), per project conventions. Errors are typed with `thiserror`;
no panics in library code.

## Consequences

- ➕ Engine is reusable + testable in isolation; front-ends are thin.
- ➕ New behaviors added by new impls, not edits to the orchestrator (OCP).
- ➕ Seams are natural mock points for unit tests.
- ➖ More crate/file boundaries and some trait boilerplate.
- ➖ A migration from the current single-binary layout (the regex collector, hasher, SQLite
   index, impact analyzer port forward; runner/pool are replaced by `exec/`).

## Alternatives considered

- **Keep single binary:** rejected — can't cleanly host both CLI and daemon, and seams get
  blurred, hurting testability.
- **Concrete types instead of traits:** rejected — violates DIP; blocks the worker/cache
  evolution the whole strategy depends on.

## Revisit trigger

If a seam never grows a second implementation after the first milestones, collapse it to a
concrete type to cut indirection (YAGNI cleanup), keeping only the seams that earned their keep
(`Worker`, `Cache` are certain to).
