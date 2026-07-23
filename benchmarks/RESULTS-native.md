# Native engine — performance (cold / warm)

> The **pure-Rust engine** (`engine/`) — the tiderace engine. Measured 2026-06-23 on this host with
> `tiderace-daemon bench` (forks per test for isolation — ADR-E003) against the `.tiderace-fx-venv`
> interpreter. Honest, not cherry-picked.

## How

```bash
cargo build -p engine-daemon --release
TIDERACE_PYTHON=.../python TIDERACE_SHIM=engine/py-shim/shim.py \
  ./target/release/tiderace-daemon bench <corpus> <iters>
```
`bench` runs the whole corpus `iters` times on one warm handler: **iter 0** includes the wellspring
launch (cold); **iter ≥1** reuse the warm wellspring (warm). pytest baselines via `python -m pytest`.

## The inner loop — warm rerun of one impacted test (the pitch)

The number that matters for the edit→save→result loop: re-run a single (impacted) test.

| | wall time | note |
|---|---:|---|
| **pytest** (1 test) | **~650 ms** | pays full interpreter + import + collection startup **every invocation** |
| tiderace **cold** (1 test) | ~382 ms | includes the one-time wellspring launch |
| tiderace **warm** (1 test) | **~7 ms** | warm imports + one `fork()`; no re-collection |

**Warm tiderace ≈ 7 ms vs pytest ≈ 650 ms — about 90× on the inner loop.** This is what the warm daemon
(ADR-E007) + impact selection (design 11) + cache (ADR-E004) buy: an edit re-runs only the impacted
test, against an already-warm interpreter — comfortably under the sub-100ms (G4) target.

## Full cold run — 511 cheap fixture tests (the honest tradeoff)

| | wall time |
|---|---:|
| pytest (full) | ~1.08 s |
| tiderace cold (full) | ~3.07 s |
| tiderace warm (full) | ~3.1 s |

On a **full run of many cheap tests, tiderace is slower than pytest.** It pays a `fork()` per test to
give pristine per-test isolation (ADR-E003) that pytest's single-process run does not provide; when the
test bodies are tiny, that ~5–6 ms/fork dominates and the one-time import tiderace amortizes is small.
Cold ≈ warm here for the same reason — the import is already a rounding error against 511 forks.

## Reading this honestly

tiderace does **not** win a from-scratch full run of a cheap suite — the fork-per-test isolation tax is
real. Its wins are structural, and compound exactly where developers spend their time:

1. **Inner loop** — warm + impact + cache means an edit re-runs *one* test (~7 ms), not the suite. This
   is ~90× pytest, which restarts cold every time.
2. **Cache** — an unchanged test isn't run at all (content-addressed, shareable across machines).
3. **Heavy-import suites** — the warm wellspring pays a multi-second import **once**, not per run; the
   bigger the import, the bigger the standing win (the fork tax stays flat).

The fork tax on cheap full runs is the cost side of the isolation guarantee — and the lever the **②
in-process backend** (ADR-E011/E013) targets next: deleting the subprocess/pipe control plane to shave
the per-test overhead while keeping fork-from-embedded isolation.
