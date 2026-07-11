# ② in-process backend — benchmark findings (the premise was wrong)

> Measured 2026-06-25. `engine/crates/engine-inproc` (PyO3 0.26 + embedded CPython 3.14.4), built with
> `PYO3_PYTHON=<venv> RUSTFLAGS="-L native=<base>/lib"`, run with
> `LD_LIBRARY_PATH=<base>/lib PYTHONPATH=<venv>/site-packages`.

## What ② set out to prove

ADR-E011 ② / the backlog ticket premised that the native engine's cost was the **per-test transport**:
each test crosses a pipe as JSON between Rust and the Python subprocess. The in-process backend embeds
**one** CPython and drives the shim's executor by **FFI call** — deleting that subprocess + pipe — so
the premise was that this would be meaningfully faster.

## What was actually measured

`InProcessTransport` works and is **correct**: it imports once, drives tests by FFI returning native
`ExecResponse` values, and — verified — keeps **fork-from-embedded isolation** (a test that mutates a
module global in its forked child does **not** leak to the next test).

But the speed premise is **wrong**:

| corpus | in-process (FFI, no pipe) | single-pipe (subprocess) | verdict |
|---|---:|---:|---|
| 500 trivial tests | ~2024 ms | ~2012 ms | **identical** |
| fx_corpus (509 fixture tests) | ~3.7 s | ~3.2 s | comparable |

**The pipe/JSON control plane is negligible (~0 ms/test).** The real per-test cost is the **`fork()`
itself** (~4 ms/test: fork + child `_child_exec` setup + body + `_exit` + parent reap), which *both*
paths pay equally. Deleting the transport changes nothing measurable, because the transport was never
the bottleneck.

## Profile: where do the ~4 ms go? (`inproc-probe bench` fork vs no-fork)

| mode | per test | 500 trivial tests |
|---|---:|---:|
| **fork-per-test** (isolated) | **4.49 ms** | ~2246 ms |
| **no-fork** (in-process) | **0.05 ms** | **~26 ms** |

The **entire** per-test cost is the `fork()` (+ child `_child_exec` + IPC + reap). Parent-side
orchestration (collection lookup, closure resolution, mark parsing) is **0.05 ms** — noise. No-fork is
**~90× cheaper**, and 500 trivial tests run in **26 ms vs pytest's 863 ms (33×)**.

**Implication:** the prize isn't *fewer* forks — it's *no* fork for tests that don't need isolation.
A **pure** test (no shared-state mutation) can run in-process at 0.05 ms; only **impure** tests need the
4.5 ms fork. The [purity guard + pure-test batching](../planning/backlog/pure-test-batching/) is the
single highest-leverage perf lever in the whole engine.

## Smart batching — the realized win (`inproc-probe smart`)

Learn each test's purity once (forked, safe), then re-run routing **pure tests to no-fork**:

| corpus | pytest | all-fork re-run | **smart (pure→no-fork)** | smart vs pytest | smart vs fork |
|---|---:|---:|---:|---:|---:|
| fx_corpus (509 fixture tests, all pure) | 800 ms | 4290 ms | **403 ms** | **2.0× faster** | 10.7× |
| 500 pure tests | 850 ms | 2758 ms | **137 ms** | **6.2× faster** | 20.2× |

The smart warm re-run **beats pytest** (2× on the fixture-heavy corpus, 6× on the pure suite) **and** the
all-fork path (10–20×) — with full per-test purity verification (every test re-snapshotted). Learn purity
once, then every subsequent run is fast. (Even the *fixture-heavy* fx_corpus is 509/509 pure — the
fixtures set up isolated state; the test *bodies* don't mutate module globals, so they're safe to run
without a fork.)

## Corrected understanding of the levers

1. **`fork()` per test (~4 ms) is the cost** — not the transport, not (for cheap tests) the body.
   Therefore the highest-leverage lever is **fewer forks**: [pure-test batching](../planning/backlog/pure-test-batching/)
   (run K pure tests per fork) directly attacks it. *Re-ranked to #1.*
2. **Parallelism** already banked the big win (pool: fx_corpus 3.27 s → 1.17 s) by running forks across
   N processes — at the cost of **N× project import**.
3. **② is only a win when import-once is combined with PARALLEL fork-out.** One embedded interpreter
   (import once) that forks **N children in parallel** would get the pool's parallelism *without* the
   N× import — beating the pool on import-heavy suites. The current `InProcessTransport` is sequential
   (the shim's `Engine.run` forks one child at a time), so it has the import-once half but not the
   parallelism half, and does **not** beat the pool. The follow-on is a **parallel-fork driver** on the
   embedded interpreter (fork from the single main thread, children run concurrently, reap via pipes).

## Status

- ✅ ② **feasibility + correctness proven** (PyO3 links 3.14; embed runs `_decimal`; FFI drives the
  executor; fork isolation real). A reusable `ShimTransport` backend exists behind the seam.
- ❌ ② **transport swap is not a perf win** on its own (this doc).
- ⏭ The win requires **parallel-fork-from-embedded** (import once + parallel) — re-scoped on the ticket.
  And independently, **pure-test batching** is now the #1 lever because the fork is the cost.

---

## The no-fork ladder (2026-06-26): isolation without paying the fork

Follow-on work. Since `fork()` (~4.5 ms/test) is the cost, the goal is to **skip the fork wherever it's
safe** — and make "safe" the engine's job, not the test author's. Three execution tiers now exist, picked
per test:

| tier | when | isolation | rel. cost |
|---|---|---|---|
| **bare no-fork** | test is *known pure* (verdict) | nothing to isolate | ~0.05 ms (90×) |
| **no-fork + restore** | test mutates a *restorable* (snapshottable) footprint | snapshot before / restore after | ~0.4–0.9 ms (5–14×) |
| **fork** | module has *opaque* globals (can't snapshot) | COW child | ~4.5 ms (1×) |

Built + proven:

- **Static pre-filter** (`shim.static_impurity`, `proof_static_purity.py`) — an AST scan that flags
  obvious mutators (`global`, write to a free/module name, env/process-global calls) **without running**.
  A sufficient (conservative) impurity test: a false "impure" only costs a fork. Seeds the tier choice
  on a cold run; distinguishes a *local* write (fine) from a *free-name* write (impure).
- **Snapshot/restore** (`Engine(restore=)`, `proof_snapshot_restore.py`) — **the big one**. Run *impure*
  tests in-process and undo their mutation from the pre-body snapshot. 50 tests that contaminate each
  other (1/50 pass un-isolated) run **50/50 isolated in 39 ms vs 207 ms forked — 5×, same isolation, and
  no learning pass** (it *contains* impurity instead of predicting it). Opaque modules auto-fall-back to
  fork (`_restorable()`), so it's sound.
- **Daemon `run` (no-fork by default)** — the win, end-to-end through the **pipe daemon** (not just the
  in-process bench). The daemon optimistically requests no-fork with `RIPTIDE_RESTORE=1`; the shim
  restores restorable modules and forks opaque ones. **No persisted verdicts needed for correctness.**
  (Originally shipped behind a `--fast` flag, now removed — it's the default; `RIPTIDE_FORCE_FORK=1` is
  the inverse.)

Measured (fx_corpus, 509 tests, **cold one-shot** through the daemon, hyperfine -r 6):

| daemon mode | mean | vs fork | note |
|---|---:|---:|---|
| `run --all` (parallel fork) | 1179 ms | 1.0× | System (fork syscalls) 3.68 s |
| `run --fast` (no-fork + restore) | **706 ms** | **1.67×** | System **0.58 s** (6× fewer syscalls) |

Cold one-shot understates it (it still pays wellspring launch + collection once fork is gone); the no-fork
fraction is larger in **warm/serve** mode where launch is amortized — there it approaches the in-process
bench's 5–20×.

### Still open

- **Free-threading (PEP 703)** — *blocked on the environment*: this box has the GIL-enabled 3.14 build
  (`Py_GIL_DISABLED: 0`), no `python3.14t`. Design holds: pure tests are thread-safe by definition, so on
  a free-threaded build they run on **threads in one interpreter** — no fork **+** parallel **+** one
  import (the trifecta). The purity guard is exactly the "is this thread-safe?" gate. Needs the
  free-threaded CPython build installed to measure.
- **Persisted purity verdicts** — correctness doesn't need them (restore + opaque-fork is sound), but
  recording verdicts lets *known-pure* tests take the **bare no-fork** tier (skip the restore snapshot →
  the full 90×). Content-address the verdict like the result cache so CI teaches every machine.
