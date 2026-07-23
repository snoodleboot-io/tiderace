# Benchmarks

tiderace ships a reproducible benchmark harness. Run it yourself rather than trusting a fixed number
— results vary by machine and, especially, by how fast your Python imports the test suite's
dependencies.

## The three-way harness

`benchmarks/bench_3way.sh` compares **pytest** vs the **old** (retired) engine vs the **native**
pure-Rust engine over the same corpus:

```bash
# defaults: corpus = benchmarks/fixtures/fx_corpus, python = .tiderace-fx-venv/bin/python
benchmarks/bench_3way.sh [corpus-dir] [venv-python]
```

It needs [hyperfine](https://github.com/sharkdp/hyperfine) and both engines built. The script sets
`TIDERACE_PYTHON` and `TIDERACE_SHIM` for you and runs three scenarios. Note how it drives the native
engine:

- **Cold full run** uses `tiderace-daemon run . --all` — no-fork + restore is the **default** path
  (no flag). The `TIDERACE_FORCE_FORK=1` variant is the debug/benchmark baseline that reverts to
  fork-per-test, so the script can show what removing the fork buys.
- **Warm no-change** uses `tiderace-daemon run .` (impact-aware) against persisted
  `.tiderace-state.json` — nothing should execute.
- **Inner loop** uses `tiderace-daemon bench <dir> 4` to time a warm rerun of one test.

## The scenarios & the measured numbers

Measured on `benchmarks/fixtures/fx_corpus` (509 fixture tests; numpy/sqlite), hyperfine. From
[`RESULTS-3way.md`](https://github.com/snoodleboot-io/tiderace/blob/main/benchmarks/RESULTS-3way.md):

| scenario | pytest | **tiderace** | speedup |
|---|---:|---:|---:|
| **Cold** — full run (all 509 execute) | 0.94 s | **0.66 s** | **1.4× faster** |
| **Warm** — no changes (impact-skip) | 0.84 s | **9.4 ms** | **89×** |
| **Warm** — inner loop, 1 changed test | 0.27 s | **~5 ms** | **~50–70×** |

## How to read it (the honest framing)

Two levers compound, and they matter in different scenarios:

- **Cold full run — tiderace now *beats* pytest (1.4×).** This is the surprising result: deleting the
  per-test `fork()` via the [no-fork ladder](../design/architecture.md#the-isolation-ladder) drops
  System time roughly 3.6 s → 0.5 s (about 6× fewer syscalls), and the snapshot/restore that replaces
  it is cheap while keeping full per-test isolation. The residual cost is the **per-worker import**
  (each pool wellspring imports the project once), not the fork.

- **Warm / impact — where tiderace dominates.** With no changes, impact-skip runs **nothing** — the
  wellspring isn't even launched — so a re-run is ~9 ms (89× pytest). A one-test inner loop is ~5 ms
  (~50–70×). This is the everyday edit→test loop, and it's the design's whole point.

!!! note "Honest framing"
    The cold full run *used* to trail pytest (pytest runs one process, isolates nothing). The no-fork
    ladder closed and then reversed that gap. But the impact loop is still where the order-of-magnitude
    wins live — fork-vs-no-fork is in the noise there because impact-skip already ran (almost) nothing.

## Real-world libraries

`benchmarks/real_world.sh` runs the comparison against the *actual* test suites of common OSS
libraries (cachetools, jmespath, toolz, inflection) — it clones them, installs them into a throwaway
venv, and times pytest vs the engine cold, warm (no change), and warm after editing one source file:

```bash
benchmarks/real_world.sh
```

The shape is the same: the warm/impact loop is dramatically faster where tests have real cost; the
cold full run is competitive-to-faster depending on import weight. Exact numbers vary by machine.

## Reproduce

```bash
# Build both engines first
cargo build --release --manifest-path engine/Cargo.toml   # native engine

# Then run the three-way harness
benchmarks/bench_3way.sh
```

Full result tables and methodology live in
[`benchmarks/RESULTS-3way.md`](https://github.com/snoodleboot-io/tiderace/blob/main/benchmarks/RESULTS-3way.md)
and [`benchmarks/RESULTS-inproc.md`](https://github.com/snoodleboot-io/tiderace/blob/main/benchmarks/RESULTS-inproc.md)
(the in-process / FFI transport experiment, which confirmed the fork — not the pipe — was the cost).
