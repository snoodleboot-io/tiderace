# ADR-E009 — Lazy assertion introspection (not import-time rewrite)

**Status:** ✅ Accepted (design)

## Context

Rich assertion failure output (the structured `assert a == b` diff) is pytest's signature UX and
non-negotiable for adoption (G5). pytest achieves it by **rewriting every `assert` statement's
AST at import time** into code that records subexpression values — paying a cost on *all*
asserts (and a bytecode-cache write) whether or not they ever fail.

Since we own the framework (E001) and fork per test (E003), we can choose a different point in
the cost curve.

## Decision

**Run asserts at native Python speed; introspect only on failure.**

1. The test body executes normally; a bare `assert` that passes costs nothing extra.
2. On `AssertionError` (or a unittest `self.assert*` failure), the engine **re-evaluates that
   single failing assertion's AST with introspection** to produce the rich diff (`RichDiff`).
3. The same `AssertionIntrospector` serves **both** bare `assert` (pytest-style) and unittest
   `self.assert*` failures — so plain `assert` gets rich diffs even inside a `unittest.TestCase`,
   which stock unittest never provides.

**Side-effect / nondeterminism guard:** re-evaluating an assert expression can re-trigger side
effects or yield a different value. The introspector:
- captures subexpression values in a **single traced re-eval**;
- if the expression is detected as impure / non-reproducing (value differs, or it performed
  I/O during re-eval), it **falls back to the plain assertion message** plus a note, rather than
  risk a misleading or harmful diff.

(Optional future: a per-file targeted rewrite, cached in Rust, for hot files where re-eval is
unsafe — but lazy is the default.)

## Consequences

- ➕ Zero assertion overhead on the happy path (the overwhelming majority of executions).
- ➕ A genuine differentiator: faster *and* richer (uniform across pytest/unittest styles).
- ➖ Re-eval of side-effecting assertions is the tricky case; handled by the purity guard +
   graceful fallback (documented limitation, not a silent wrong answer).
- ➖ The introspector must parse/evaluate Python AST fragments — implemented in the shim where
   Python's `ast` is available, orchestrated by Rust.

## Alternatives considered

- **Import-time AST rewrite (pytest's approach):** no re-eval hazard, but pays on every assert
  and complicates our fork/wellspring import path — rejected as default (kept as the optional
  per-file fallback).
- **No introspection (plain `AssertionError`):** unacceptable UX — rejected.

## Revisit trigger

If the purity guard's fallback fires too often on real suites (many side-effecting asserts),
promote the cached per-file targeted rewrite from "optional future" to default for affected
files.
