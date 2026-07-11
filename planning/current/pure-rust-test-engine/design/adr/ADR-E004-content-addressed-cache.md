# ADR-E004 — Content-addressed result cache (build-system-for-tests)

**Status:** ✅ Accepted (design) · Supersedes old impact-only skipping (old ADR-011 feeds it).

## Context

The fastest test is the one we never run. The old design only *skipped* tests whose
dependencies hadn't changed (impact analysis). That is local-only, run-relative, and not
shareable. We want Bazel/Nix-grade memoization: a test outcome is a **pure function of its
inputs**, cached by content hash, and **shareable across machines and CI**.

The hard problem is **soundness**: Python tests can be impure (clock, network, RNG, filesystem),
so a naive cache returns stale/incorrect results.

## Decision

Model each test outcome as a content-addressed artifact.

**Cache key** = hash over the test's *transitive input closure*:

```
key = H(
    test_bytecode
  + executed_source_closure   # from coverage (E006), not static guesses
  + fixture_closure           # fixture bodies + their transitive deps
  + declared_env              # env vars / files the test is allowed to read
  + engine_version + python_version + platform
)
```

**Store:** a content store (outcome + captured stdout/stderr/diagnostics) with a SQLite index;
**tiered** `LocalCache` + optional `RemoteCache` behind a `TieredCache` (E005 seams).

**Soundness strategy (staged, conservative-by-default):**
1. The **executed-source closure comes from coverage** (E006), so it reflects what the test
   *actually* touched, not a static guess.
2. **Sandboxed observation** (staged) intercepts fs/env/network/clock/RNG to *discover* the true
   input set and *detect impurity*.
3. **Impure tests are marked uncacheable** (or their nondeterministic inputs are pinned —
   frozen clock, seeded RNG, recorded network) — never silently cached.
4. **Bootstrap:** a test never seen on this content has no closure yet → it runs, and that run
   produces the closure for next time.

Preference order enforced by the orchestrator: **cache hit → impact-skip → run**.

## Consequences

- ➕ Inner-loop and CI runs approach O(changed tests); full cache hit → near-zero.
- ➕ Remote cache means a green test someone already ran is free on a fresh machine/CI shard.
- ➖ Correctness depends on closure completeness → we default conservative (coverage-derived
   closure + impurity detection) and make aggressiveness opt-in.
- ➖ Requires the coverage/sandbox machinery; impurity detection is non-trivial (staged).
- ➖ Cache invalidation must include engine/python/platform to avoid cross-environment poisoning.

## Alternatives considered

- **Impact-skip only (old ADR-011):** weaker, local-only, not shareable — subsumed as a
  fallback layer.
- **Timestamp/mtime-based:** unsound (mtime ≠ content) — rejected.
- **Trust user `@pure` annotations only:** too error-prone as the *primary* mechanism; used only
  as an optional aggressiveness hint.

## Revisit trigger

If sandbox-based impurity detection proves too costly or too leaky, narrow caching to an
explicit opt-in (`@cacheable` / pure-by-declaration) while keeping impact-skip as the always-on
default.
