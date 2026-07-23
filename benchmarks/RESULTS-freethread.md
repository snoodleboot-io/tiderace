# Free-threading (PEP 703 / CPython 3.14t) — evaluation findings (TID-3)

> Measured 2026-07-21 on CPython **3.14.4** in two builds: the ordinary GIL-enabled interpreter and the
> free-threaded `3.14t` build (`uv python install cpython-3.14.4+freethreaded`; `Py_GIL_DISABLED=1`,
> `sys._is_gil_enabled()` → `False`). Reproduce with `benchmarks/freethread_spike.py` under each
> interpreter. numpy measured from a free-threaded venv (`uv venv --python <3.14t>`; numpy 2.5.1 built
> from source — no cp314t wheel yet, ~2 min compile).

## What this evaluated

Whether a free-threaded interpreter can serve as a **parallel no-fork tier**: run pure tests on
**threads** in one process — no `fork()`, one import, parallel across cores. This is the same problem
the sub-interpreter tier (TID-2 / ADR-E015) solves, approached from the other side, so the evaluation
also asks how the two relate.

## What was measured

| Signal | GIL-enabled 3.14 | free-threaded 3.14t |
|---|---:|---:|
| Parallel speedup (8 workers, CPU-bound pure bodies) | **0.87×** | **3.87–4.19×** |
| Pure (read-only) tests correct under concurrency | ✅ | ✅ |
| snapshot/restore races on the shared module dict | no† | **yes** (counter left corrupt: 19–28 ≠ 0) |
| numpy imports **and keeps the GIL off** | n/a | ✅ (2.5.1, `_is_gil_enabled()` stays `False`) |

† The GIL serialises the mutate→restore window, so the race is *masked*, not absent — it reappears the
moment the GIL is off. That is the point: the isolation the no-fork ladder relies on is GIL-dependent.

## The three findings

**1. The parallelism is real.** 0.87× → ~4× is the whole thesis confirmed: with the GIL off, pure
Python CPU-bound bodies scale across cores on threads. The 0.87× GIL-enabled figure (a slight *loss*
from thread-scheduling overhead) is the baseline that makes the point — threading buys nothing until
the GIL is gone.

**2. Only *genuinely pure* tests are thread-safe — the restore rung is unavailable.** The no-fork
ladder isolates an *impure-but-restorable* module by snapshotting its globals, running the test, and
restoring. That is a read-modify-write on the module dict, and the module dict is **shared across all
threads**. Experiment 3 shows the interleaving corrupts state once the GIL is off. So free-threading
cannot use the restore rung at all: it can run tests that *never mutate shared state*, and nothing
else. The purity guard (TID-1) is exactly this gate — but here it must mean *strictly pure*, not
*restorable*, a stricter bar than the fork ladder demands.

**3. numpy is the differentiator over sub-interpreters.** TID-2's universal sub-interpreter backend was
a NO-GO because numpy's `_multiarray_umath` refuses to load in an isolated sub-interpreter. Under
free-threading numpy 2.5.1 loads **and leaves the GIL disabled** — so a pure numpy/pandas/scipy test
*can* run parallel-no-fork here, which it never could in a sub-interpreter. The catch is symmetric: an
extension that does *not* declare free-threading support re-enables the GIL on import
(`sys._is_gil_enabled()` → `True`), silently collapsing the whole tier back to 0.87×. It is a
per-dependency property that must be probed, exactly like sub-interpreter safety.

## Verdict — conditional GO, complementary to the sub-interpreter tier, deferred

Free-threading is a **real** parallel-no-fork tier, not a dead end. But it is **narrow** and lands as a
peer of ADR-E015, not a replacement:

| | sub-interpreter tier (TID-2, shipped) | free-threading tier (this eval) |
|---|---|---|
| isolation | per-interpreter module dict + `os.environ` | **none** — shared process state |
| admits | sub-interp-safe modules (incl. *impure-restorable*) | **strictly pure** tests only |
| numpy / C-extensions | **excluded** (can't load isolated) | **included** *if* the ext is FT-ready |
| parallelism | per-interpreter GIL (~2.9×) | no GIL (~4×) |
| runtime cost | none (stdlib 3.14) | a separate `3.14t` build + FT-ready deps |

They cover **disjoint** cases: the sub-interp tier parallelises impure-restorable stdlib modules but
not numpy; free-threading parallelises pure numpy but not impure-anything. A complete story would probe
both axes and route per-module.

**Why deferred, not built now:**

- **Strictly-pure is a small slice.** The fork ladder already runs pure tests bare-no-fork at ~90×
  single-threaded; free-threading's win is *parallelism* on top of that, which the fork pool already
  provides on Linux. The unique gain is again **Windows** (no fork) — and there it overlaps the
  sub-interpreter tier we just shipped, for a strictly smaller set of tests (pure-only vs safe).
- **It needs a second toolchain.** A `3.14t` interpreter *and* an all-FT-ready dependency set. One
  GIL-re-enabling extension anywhere in the import graph reverts the tier to a no-op, undetected unless
  probed. That probe + a second interpreter to provision is real operational weight for the slice it buys.
- **The soundness bar is subtle.** "Pure" here must mean *never touches shared state*, stricter than the
  ladder's *restorable*. Getting that wrong is a silent data race, not a crash — the worst failure mode,
  and precisely the class of bug this session kept surfacing.

**Recommendation:** keep TID-3 as a recorded GO-but-deferred. Revisit if/when (a) Windows parallelism
of pure numpy suites becomes a concrete ask, and (b) the scientific-Python stack ships FT-ready wheels
broadly (numpy already does; the long tail does not). The build blocker that parked this is gone —
`uv` fetches `3.14t` on demand — so the gate now is **value**, not availability.

## Reproduce

```bash
FT=$(uv python find cpython-3.14.4+freethreaded)   # uv python install cpython-3.14.4+freethreaded first
.tiderace-fx-venv/bin/python benchmarks/freethread_spike.py   # GIL-enabled baseline
"$FT"                       benchmarks/freethread_spike.py   # free-threaded
# numpy leg: uv venv --python "$FT" <dir> && uv pip install --python <dir>/bin/python numpy
```
