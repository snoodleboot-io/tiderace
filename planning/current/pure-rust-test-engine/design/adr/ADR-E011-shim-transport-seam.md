# ADR-E011 — Shim transport seam (and the in-process backend it unlocks)

**Status:** ✅ Accepted + implemented (the seam) · 🟡 Proposed, awaiting human ratification (the in-process / FFI backend)

**Relates to:** [ADR-E002](ADR-E002-execution-substrate.md) (execution substrate), [ADR-E005](ADR-E005-workspace-trait-seams.md) (trait seams), [ADR-E008](ADR-E008-cross-platform.md) (no-fork fallback). **Supersedes nothing.** **Does NOT reopen [ADR-010](../../../../../docs/design/decisions.md) — see "Not the subinterpreter path."**

## Context

How the Rust kernel drives CPython and how the two halves *talk* were welded together. `Wellspring`
(fork path) and `SubprocessWorker`'s `NoForkProc` (no-fork path) each **hand-inlined** the same two
things: the length-prefixed-JSON exchange over `ChildStdin`/`ChildStdout`, and — in the two `Worker`
impls — the per-item run loop (`build ExecRequest → write/read frame → assemble TestResult`). Three
copies of one loop, two copies of one pipe dance, all soldered to a real OS process.

The cost was **testability**, not just duplication. Every execution-path test had to `fork`/`exec` a
real interpreter against a live `.riptide-fx-venv`; the acceptance scenarios early-return `SKIP` when
that venv is absent (`fixtures_acceptance.rs`). So in CI-without-Python the entire
`Worker → frames → TestResult` path — scheduling, batching, result mapping, mid-run-failure handling —
was **unverified**. "We're building our own framework but can only prove the executor works by shelling
out to another one" is the corner. The fixture/watermark *planning* layer is already pure-Rust testable
(no Python); the *execution* layer had no such seam.

Conflated underneath "syscall nonsense" were two separable boundaries: **(a) the execution boundary**
(Rust spawns CPython) and **(b) the transport** (JSON frames over pipes). Naming (b) is what lets us
vary it — including to *no syscalls at all*.

## Decision

Introduce **`ShimTransport`** — the one thing a `Worker` needs from the world below it: a synchronous
request→response exchange (`ready()` + `exchange(&ExecRequest) -> ExecResponse`). The per-item loop
becomes a single `run_batch(transport, items, deadline)`; both workers delegate to it.

```text
Worker::run
  └─ run_batch(&mut impl ShimTransport, items, deadline_ms)   // the one loop
        └─ ShimTransport
             ├─ PipeTransport<W, R>   → frames over a child process's pipes   (production)
             ├─ ScriptedShim          → in-thread, struct→struct, no bytes     (tests, fastest)
             ├─ LoopbackShim          → real framing over std::io::pipe + a    (tests, wire codec)
             │                          Rust "fake shim" thread, no fork/exec
             └─ InProcessTransport    → FFI into one embedded interpreter      (PROPOSED, ②)
```

| Layer | Today | After |
|---|---|---|
| Execution boundary | Rust `spawn`s `python shim.py` | unchanged (this ADR) |
| Transport | pipes, hand-inlined ×2 | `ShimTransport`; `PipeTransport` is the prod impl |
| Per-item loop | copy-pasted ×3 | `run_batch`, once |
| Offline test of executor | impossible (needs venv) | `ScriptedShim` / `LoopbackShim`, zero syscalls |

`PipeTransport<W: Write, R: Read>` is generic over the byte streams precisely so an in-memory pipe can
stand in for the process — the production type alias is `PipeTransport<ChildStdin, BufReader<ChildStdout>>`.
Result-identity is preserved: the live fork and no-fork acceptance scenarios still pass unchanged.

Code style per project conventions: `transport.rs` holds the seam; typed `EngineError`, no panics.

### The in-process / FFI backend this unlocks (② — PROPOSED)

`ShimTransport` is the seam a future **`InProcessTransport`** plugs behind with **no `Worker` change**:
ship the kernel as a compiled extension (PyO3/maturin, abi3) loaded **into one** Python interpreter and
drive **riptide's own executor** (calling user test/fixture bodies — never pytest, per ADR-E001) by
**function call** instead of pipe frame — the JSON-framing-over-pipes control plane
disappears, `fork()` is retained only where it earns isolation. This is the genuinely "rust-native"
execution path. It is **flagged for human ratification** and should land as its own follow-up once the
seam (above) has proven out.

**Feasibility: proven (GO).** A throwaway PyO3 0.23 spike ([`spike-inproc/`](../../../../../spike-inproc/RESULTS.md),
not in engine-core) embeds one CPython 3.11.15 and, by FFI, drives **riptide's own executor — no pytest**:
it imports the user module, calls the bare `test_*` bodies, catches `AssertionError` (exactly as
`engine/py-shim/shim.py` does), and drives stdlib `unittest.TestCase.run()` — per-test verdicts extracted
as Rust values, the exact `exchange` shape. Critically it imports and hammers **`_decimal`**, the precise
single-phase-init C-ext that core-dumped ADR-010's *subinterpreters*, with **no crash** — confirming that
hazard is multi-interpreter-only. The ADR-010 "missing headers" wall was incidental (a uv standalone ships
`libpython` + headers, no sudo).
**The open question ② must answer is therefore NOT "can we embed" (yes) but "isolation under an embedded
interpreter"** — fork-from-embedded (retain the watermark model) vs. per-test module reset. ② replaces
the pipe/JSON *control plane*, not the fork-based *isolation*.

### Not the subinterpreter path

[ADR-010](../../../../../docs/design/decisions.md) rejected **N PEP-684 subinterpreters per process**
because single-phase-init C extensions segfault the whole process. The ② backend is **one main
interpreter + an FFI control plane** — the same single-interpreter, single-GIL world every pytest plugin
already runs in. It has **none** of ADR-010's failure mode and does not reopen that decision. The
rejection of *many* interpreters per process is not a rejection of *one*.

## Consequences

- ➕ The executor (loop, batching, result mapping, mid-run-close handling) is now verified **offline,
  deterministically, in CI without Python** — `ScriptedShim`/`LoopbackShim` tests.
- ➕ Live differential/acceptance tests are now an **enrichment tier**, not the only gate; a Python-less
  CI still meaningfully tests execution.
- ➕ Net **deduplication**: one transport, one run loop (was 2 + 3 copies).
- ➕ ② becomes "add a third `ShimTransport` impl," not "rewrite the executor."
- ➖ One more abstraction between `Worker` and the pipe (a trait + a `run_batch` indirection).
- ⚠️ The in-process fakes test *riptide's logic*, **not** Python semantics — the live tier still owns
  "does pytest actually behave." Do not let the fast offline tier lull us into dropping the live one.

## Alternatives considered

- **Leave it inlined, keep skipping when venv is absent:** rejected — the headline execution path stays
  unverifiable offline, and ② would have no seam to land behind.
- **Mock CPython / a fake `python` binary on `PATH`:** rejected — still pays `fork`/`exec`, still flaky,
  and tests the OS plumbing rather than the engine logic we care about.
- **Jump straight to the FFI backend (skip the seam):** rejected — bigger, riskier, and unratified;
  the seam is the cheap reversible step that de-risks it and pays off on its own.

## Revisit trigger

If `InProcessTransport` (②) ships and the subprocess `PipeTransport` is only ever used as the
cross-platform/no-fork fallback, keep both (they earn their keep, like `Worker`/`Cache` in ADR-E005). If
② is *not* pursued, the seam still stands on its testability win alone — do not collapse it back.
