# ADR-E007 — Warm daemon as the primary execution host

**Status:** ✅ Accepted (design) · Generalizes the old `pool.rs` watch-mode pool.

## Context

Inner-loop latency is dominated by interpreter + import warmth and by re-collection. The old
design only kept warm workers during `tiderace watch`. To hit the sub-100ms edit→result target
(G4), warmth must be the *normal* state, available to ad-hoc CLI invocations and IDEs alike.

## Decision

Introduce a long-lived **daemon** (`engine-daemon`) as the primary host of warm state:

- Holds warm **wellspring(s)** (E003), the **caches** (E004), and **collection state**.
- Speaks **JSON-RPC** to two clients: the thin CLI and IDE/test-explorers.
- Watches the filesystem; on change it **diffs** (AST/hash), runs **impact** (11), and
  fork-runs only the impacted, non-cached tests.
- Is the **single source of truth** for warm state; the CLI becomes a thin client that starts /
  reuses the daemon transparently.

Cold/one-shot use still works (CLI can run an ephemeral in-process engine), but the daemon is
the fast path.

## Consequences

- ➕ Sub-100ms inner loop becomes reachable: warm imports + cache + impact + fork.
- ➕ First-class IDE integration (discover/run/stream over JSON-RPC).
- ➖ A stateful long-lived process: needs lifecycle management (start/stop/health), memory
   bounds, and **robust module invalidation** when files change (stale-import bugs are the
   classic failure mode — invalidate via content hash, recycle wellspring when `conftest`/config or
   C-level state changes).
- ➖ Security/scoping: daemon is per-project, per-user, on a local socket only.

## Alternatives considered

- **Cold CLI every run:** simplest, but cannot hit the latency target — rejected as default.
- **Watch-mode-only pool (today's `pool.rs`):** narrower; doesn't help ad-hoc runs or IDEs —
  generalized into the daemon.
- **Editor-plugin-embedded engine:** ties warmth to one editor; rejected in favor of a shared
  daemon any client can use.

## Revisit trigger

If daemon state-management bugs (stale modules, memory growth) outweigh the latency benefit,
fall back to a per-invocation warm pool with aggressive wellspring reuse keyed by content hash.
