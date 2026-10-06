# Benchmark harness — parity first, then wall clock

The package behind the *Tiderace on Eight Suites* benchmark document — every pass is `python -m benchmarks.harness.<pass>` from the repository root. `run_benchmarks.py` one
directory up measures a generated fixture under hyperfine; this directory measures **real suites**
— vendored public projects and a snapshot of the internal monorepo — and, before it times
anything, checks that tiderace agrees with pytest on them test for test.

| script | what it does |
| -- | -- |
| `corpora.py` | the corpus table (`Corpus` records); every pass imports it |
| `runs.py` | the timed run, the interleaved rounds, and the two command lines, spelled once |
| `reports.py` | where each pass's JSON lands, and how it is written and read back |
| `parity.py` | pytest vs tiderace tallies per corpus, from `tiderace run --report` |
| `nodediff.py` | per-node outcome diff for one corpus — the only sound comparison |
| `selection_diff.py` | what `-k` / `-m` select, as node-id sets, against pytest's `--collect-only` (TID-100) |
| `platform_bench.py` | cold timings on Windows / macOS / Linux for the corpora that need no snapshot, each tier the platform has; run by `bench-platforms.yml` (TID-13) |
| `scale_corpus.py`, `scale_bench.py` | a synthetic suite of any size (`--async-share` makes part of it `async def`, for tiderace alone), and how the runners scale with it — full run, `-k` one test, with and without the daemon |
| `timing_rr.py` | pytest / `pytest -n auto` / tiderace, interleaved rounds, medians, load recorded |
| `binab.py` | two tiderace binaries A/B'd on the same corpora, interleaved |
| `shimab.py` | the shim at several git refs A/B'd with one binary, interleaved — **the gate every shim change runs against its merge base** (TID-126); exits 1 past the threshold |
| `bench_diff.py` | two `timing_rr` passes side by side: medians, change, the ratios — read before any table is updated |
| `ladder_bench.py` | the isolation ladder's cost per test, per tier, on a sync and an async synthetic suite |
| `analyse_bins.py` | rebuild the scheduler's bins from a report and charge them measured durations |
| `second_run.py` | the run after an edit: warm no-change, leaf edit, hub edit, injected failure (TID-65) — pytest and pytest-testmon as the comparisons |
| `warm_vs_xdist.py` | tiderace on its second run (duration-ordered) against `pytest -n auto` (TID-52) |
| `timeline.py` | draw a run's schedule from `--report`: one lane per worker, idle, critical path, ideal makespan (TID-78) |
| `quiet_gate.sh` | wait for the machine to be quiet before a timed pass |

## Method

- **One environment per suite.** Both runners use the same virtualenv and the same target
  directory. Nothing is installed for one and not the other.
- **Pinned inputs.** Public projects are the vendored checkouts under `conformance/vendor/`. The
  internal suites run against a snapshot of the source with a copy of its virtualenv
  (`PIRN_SNAPSHOT`), because the live checkout's venv was once being installed into *during* a
  pass, which moved pytest's own totals between runs.
- **Parity before speed.** A runner that is fast and wrong is not fast. `parity.py` requires
  passed / failed / error to agree exactly; `nodediff.py` compares node-id sets, since a tally
  hides two errors that cancel — a 62-test gap once decomposed into 36 changed outcomes plus 32
  ids that existed on one side only plus 6 extra.
- **Interleaved timing.** Each round runs every tool once, rotating which goes first; one warm-up
  round is discarded; the median is reported. Running all of one tool's repeats and then the
  next's hands whichever ran during a busy stretch a worse number.
- **Load recorded, not assumed.** Every sample stores the one-minute load average it ran under, and
  `quiet_gate.sh` refuses to start a timed pass on a saturated machine.

## Set-up

```bash
cd engine && cargo build --release -p engine-cli && cd ..
# one venv per public corpus, with that project's pinned pytest and pytest-xdist
# (`uv`; a Debian python3 without python3-venv cannot `python -m venv`):
for c in cachetools click flask anyio; do
  uv venv -q .tiderace-bench-venvs/$c
  uv pip install -q --python .tiderace-bench-venvs/$c/bin/python -e conformance/vendor/$c pytest pytest-xdist
done
python -m benchmarks.harness.parity cachetools        # 215 passed on both sides
```

For the internal corpora, snapshot the monorepo at a commit (source plus a *copy* of its venv,
with the `.pth` entries rewritten to the copy) and point `PIRN_SNAPSHOT` at it. The snapshot stays
a faithful copy of the project's environment, so pytest-xdist — the benchmark's comparison, not the
project's dependency — is installed beside it and reached through `PYTHONPATH`:

```bash
uv pip install --python $PIRN_SNAPSHOT/.venv/bin/python --target .tiderace-bench-venvs/xdist pytest-xdist
uv pip install --python $PIRN_SNAPSHOT/.venv/bin/python --target .tiderace-bench-venvs/testmon pytest-testmon
```

pytest-testmon is supplied the same way (`TESTMON_PATH`) for `second_run`, which runs it with
`--testmon-forceselect` (it switches selection off when `-m` is in play, and the monorepo suites
deselect their slow marks in `addopts`) and `COVERAGE_CORE=ctrace` (on 3.14 coverage's default
core records no dynamic contexts, and testmon then records nothing).

## The gate every shim or engine change runs

The correctness gates (`parity`, `nodediff`, the live Rust suites) time nothing, and absolute
timings from this shared box are not comparable across days — only an interleaved A/B is. So a
change to the shim or the engine's hot path runs, before it merges:

```bash
benchmarks/harness/quiet_gate.sh 8 python -m benchmarks.harness.shimab base=origin/main cand=HEAD anyio pirn-core fx_corpus
```

`shimab` exports `engine/py-shim` and `engine/py-tiderace` at each ref (`git archive`), runs the
one release binary with `TIDERACE_SHIM` and `PYTHONPATH` pointed at each tree, rotates the arms
each round, discards the warm-up, reports the median and the load, writes `shimab.json`, and exits
1 when an arm is more than `THRESHOLD` (10%) slower than the first. `ROUNDS` sets the rounds (4 is
enough to see a 2× and to reject one load outlier). A Rust change runs `binab` with the two builds
instead. A change to the isolation ladder also runs `ladder_bench --python .tiderace-fx-venv/bin/python`,
the ladder's cost per test, per tier, sync and async.

The pass that reproduces the finding this was built for: `shimab main=origin/main fix=HEAD step3=f04026f anyio`
showed main at 27 s, the step-3 shim at 13 s, and the fix at 15 s (TID-125).
`ladder_bench` reproduces it on the synthetic async suite, whose fixture tests hand one call to
`asyncio.to_thread` so the loop's executor leaves an idle pool behind: the ladder cost 3,219 µs
per async test before the fix and 933 after (one worker: 9,298 → 1,932); sync was flat at ~620.
