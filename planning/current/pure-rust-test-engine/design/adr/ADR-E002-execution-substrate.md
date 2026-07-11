# ADR-E002 — Execution substrate: subprocess + Python shim

**Status:** ✅ Accepted (design) · Builds on old **ADR-001** (subprocess) and old **ADR-010**
(subinterpreters rejected).

## Context

The engine must execute Python bodies. Options for the Rust↔Python boundary:

1. **PyO3-embedded** — link libpython into our binary; drive CPython in-process.
2. **Subprocess + shim** — spawn plain `python` running a tiny Rust-shipped shim; talk over a
   pipe.
3. **Subinterpreters (PEP 684)** — many interpreters in one process.

Constraints: full C-extension compatibility (numpy, pydantic-core, DB drivers); the fork model
(E003) needs a clean parent interpreter; we want a small, portable, ABI-stable boundary.

## Decision

Use **subprocess workers running plain `python` + a tiny Rust-shipped shim**
(`crates/py-shim/shim.py`), communicating over a **binary length-prefixed pipe protocol**
(bincode/msgpack), not newline-JSON.

- The shim is deliberately dumb: "import module M, build fixture args, call callable C, capture
  outcome, stream events." All policy stays in Rust.
- **No libpython linking in the default path.** PyO3-embedded is kept as a *possible future
  optimization* if the IPC boundary ever dominates, but is not the baseline.
- **Subinterpreters: rejected**, consistent with old ADR-010 — most C extensions are not
  multi-interpreter-safe and crash the process. PEP 684/734 do not change this materially yet.

The **wellspring/fork** model (E003) sits *on top* of this substrate: the wellspring is one of these
Python processes that has imported the project; fork workers are its COW children running the
same shim.

## Consequences

- ➕ Full C-extension compatibility and OS-level isolation.
- ➕ No libpython ABI/version/link matrix to manage across CPython releases.
- ➕ Clean parent for fork (no half-initialized embedded runtime).
- ➖ IPC cost per message vs in-process calls → mitigated by binary protocol + batching +
   streaming events; the heavy path (import) is amortized by the wellspring anyway.
- ➖ We ship and version a Python shim file (kept tiny; covered by conformance tests).

## Alternatives considered

- **PyO3-embedded (baseline):** rejected for now — tighter control but libpython linking pain,
  and fork-after-embedded-init is delicate. Reconsider only if IPC is shown to dominate.
- **Subinterpreters:** rejected — C-ext safety (old ADR-010 still holds).
- **gRPC/HTTP boundary:** rejected — heavier than a local pipe for no benefit.

## Revisit trigger

Benchmarks showing IPC/serialization is a top-3 cost on realistic suites → prototype a
PyO3-embedded `ForkWorker` variant behind the same `Worker` trait and compare.
