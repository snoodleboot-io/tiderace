# Benchmarks

Every number here comes from a harness in the repository; run it rather than trust it. Results
vary by machine and, above all, by how fast your Python imports your suite's dependencies.

## The short version

Eight real suites — four public projects vendored at fixed commits, four internal ones from a
pinned monorepo snapshot — run by pytest and by tiderace in the same virtualenv, compared **test
by test** before anything is timed. As of the 29 September pass on main at `ad3481f`:

| | |
| -- | -- |
| parity | identical outcomes on seven of eight suites, node id for node id; the eighth (anyio) collects the same 1,479 nodes and differs on ten outcomes, eight of them tests that drive pytest itself |
| a full run, against serial pytest | 2.0× to 5.0× faster on seven suites |
| a full run, against `pytest -n auto` | 1.5× on the two 5,000-test suites; more on small suites, where xdist's start-up is the whole run |
| a warm run with nothing edited | 0.16s and 0.13s on the two 5,000-test suites, against pytest's 80s and 106s |
| one leaf module edited | 1.4s and 1.8s |
| one test by name through a warm daemon | 0.6s on a 5,600-test suite (5s without the daemon) |

The published document with the full tables, the method and every caveat is
[Tiderace on Eight Suites](https://claude.ai/artifact/WNYBgGqwbtQuW9iW14EyBn); the sections below
carry the same numbers and the history of how they moved.

## The fixture microbenchmark

`benchmarks/bench_3way.sh` compares **pytest** vs the **old** (retired) engine vs the **native**
engine over the repository's own 509-test fixture corpus (`benchmarks/fixtures/fx_corpus`;
numpy/sqlite), with [hyperfine](https://github.com/sharkdp/hyperfine):

```bash
# defaults: corpus = benchmarks/fixtures/fx_corpus, python = .tiderace-fx-venv/bin/python
benchmarks/bench_3way.sh [corpus-dir] [venv-python]
```

Read it as a microbenchmark of the engine's own overheads, not as a suite: fx_corpus is one
500-test file, and since TID-80 a file runs whole on one worker, as under pytest — so the default
full run is serial, **0.95s against pytest's 0.83s**, and `--shard-modules` (for suites whose
files are known independent) runs it in 0.56s. The warm and inner-loop rows the script also
measures (a no-change run in single-digit milliseconds, one changed test in ~5 ms) are the
impact-skip path and hold; the real-suite equivalents are in the tables below.

## Real suites: parity first, then the second run

`benchmarks/harness/` runs the comparison against **real** test suites — the vendored public
projects under `conformance/vendor/` and a pinned snapshot of the internal monorepo — and, before
it times anything, checks that tiderace agrees with pytest on them test for test. The published
benchmark document is produced from it.

```bash
python benchmarks/harness/parity.py             # pytest vs tiderace tallies, per corpus
python benchmarks/harness/nodediff.py click     # per-node outcome diff — the only sound comparison
python benchmarks/harness/timing_rr.py          # pytest / xdist / tiderace, interleaved, medians
PIRN_SNAPSHOT=... python benchmarks/harness/second_run.py pirn-core   # the run after an edit
```

The method — pinned inputs, parity before speed, interleaved rounds, load recorded, node-id
comparison — is in that directory's README. `second_run.py` is the benchmark the cold numbers above
do not cover: a warm run with nothing edited, one edit to a leaf module, one to a hub module, and an
edit that must produce a failure (the stale-pass check). Its numbers are in the section below.

### The eight-suite pass, 29 September 2026

`bench_pass.sh` — parity, node diff, cold timings, the second run, warm vs xdist — on main at
`ad3481f` (after TID-76 … TID-86), release build, defaults. Machine shared: one core was pinned by
an unrelated process throughout, one-minute load 2.5–8.5 recorded with every sample; three
interleaved rounds after a discarded warm-up, medians.

**Parity first.** Per-node outcome diff (`nodediff.py`) against pytest in the same venv:

| suite | tests | outcome differences | node ids only one side has |
| -- | --: | --: | -- |
| fx_corpus | 511 | 0 | 0 |
| pirn-data | 1,463 | 0 | 0 |
| pirn-core | 5,036 | 0 | pytest's `addopts` deselections (tiderace reports them as skipped) |
| pirn-agents | 4,652 | 0 | the same |
| cachetools | 215 | 0 | 0 |
| click | 589 + 1 xfail + 21 skipped | 0 | 0 |
| flask | 475 + 4 failed + 3 error + 2 skipped (both sides) | 0 | 0 |
| anyio | 1,479 | 10 tiderace-only failures, 8 of them `pytester`'s `testdir` (the plugin-host boundary) | 0 |

The click and flask "skipped" rows read as tiderace-only until [TID-88](https://linear.app/snoodleboot/issue/TID-88):
`nodediff.py` took pytest's side from `-rA`, whose summary folds every skip into a `SKIPPED [16]
file:line` line with no node id, so pytest's own skipped variants were invisible to the diff. It now
reads `-v`, one line per node. anyio went from 222 failed/error and 332 / 384 ids only one side has
(TID-86) to 0 / 0 ids and 31 failed/error after TID-87 (plugin fixtures) and TID-88 (a parametrized
fixture name not in the signature is indirect; duplicate ids take pytest 8's `_` suffix; a skip-marked
parametrized test is skipped per variant; a fixture named `test*` is not a test).

**Cold timings**, median wall clock in seconds. tiderace here is `tiderace run` with durations
already recorded from an earlier run, so its work units are cost-ordered; xdist is `pytest -n auto`.

| suite | pytest | pytest -n auto | tiderace | vs pytest | vs xdist |
| -- | --: | --: | --: | --: | --: |
| pirn-core | 77.9 | 40.4 | **26.7** | 2.92× | 1.51× |
| pirn-agents | 105.9 | 47.6 | **31.1** | 3.40× | 1.53× |
| pirn-data | 31.2 | exit 3 | **15.3** | 2.04× | — |
| anyio | 48.2 | 14.9 | **9.7** | 4.97× | 1.54× (partial parity, see above) |
| cachetools | 0.57 | 1.58 | **0.29** | 1.97× | 5.4× |
| click | 1.36 | 1.82 | **0.56** | 2.43× | 3.3× |
| flask | 2.08 | 2.40 | **0.99** | 2.10× | 2.4× |
| fx_corpus | 0.83 | 2.19 | 0.95 (**0.56** with `--shard-modules`) | 0.87× (1.48× sharded) | 2.3× |

fx_corpus is one 500-test file: since TID-80 a file runs whole on one worker, as under pytest, so
the default run is serial and slower than pytest; `--shard-modules` (for suites whose files are
known independent) splits it and runs it in 0.56s. The public suites' xdist rows are its start-up
on suites that finish in a second or two; the
honest head-to-heads are the two 5k-test monorepo suites at 1.5×. On pirn-agents the earlier pass
had tiderace *behind* xdist (0.77×); the queue (TID-52), cost-ordered units (TID-62) and the
selective/shared import (TID-75/76) turned that into 1.53× — the same suite, same venv.
`warm_vs_xdist.py`, the dedicated head-to-head at the same load, agrees on pirn-core (**xdist
40.2s, tiderace 26.8s, 1.50×**); on pirn-agents its two rounds were 61.0s and 31.3s against xdist's
47.2s — the 61s round is an outlier with no recorded cause (the three cold-pass rounds sat at
31.0–31.4s), and it is reported rather than dropped.

### The second run, measured

`second_run.py` on the two ~5k-test monorepo suites, same pass: a warm run with nothing edited,
one edit to a leaf module, one to the hub module, and an edit that must produce a failure. pytest
has no warm mode, so its number is the same full run every time — that is the comparison.

| scenario | pirn-core | pirn-agents |
| -- | -- | -- |
| pytest, full run | 80.0s | 105.8s |
| tiderace, cold `run --all` (coverage on, footprints recorded) | 30.7s | 40.3s |
| tiderace, warm, **nothing edited** | **0.16s** — 0 ran, 5,602 cached | **0.13s** — 0 ran, 4,657 cached |
| edit one leaf module (4 / 1 dependents) | **1.38s** — 4 ran | **1.79s** — 1 ran |
| edit the hub module (3,958 / 2,428 dependents) | 25.9s — 3,793 ran | 13.9s — 2,316 ran |
| leaf module made to raise on import | 4 failing, **reported** | 1 failing, **reported** |

pirn-agents' warm rows carried one cached failure from the daemon's own cold run in this pass:
`tests/llm/test_cross_process_provider_replay.py::TestCrossProcessProviderReplay::test_every_scenario_really_called_the_mock_server_while_recording`,
which passes in the one-shot run of the same suite. Not scheduling: the daemon handed its pool a
5 s per-test deadline where `tiderace run` allows 60 s, and that class's set-up starts a mock HTTP
server and runs two worker interpreters. One deadline for both since
[TID-89](https://linear.app/snoodleboot/issue/TID-89); the daemon's full run on pirn-agents is
`4657 tests, 0 failing` and the warm run `0 ran, 4657 cached, 0 failing`.

The first measurement of this table (20 September) read 7.6s / 2.3s for the no-change row and
8.1s / 2.3s for a leaf edit; the bullets below record what each step found. The cold `run --all`
is now 30.7s against the 51.8s it was: the coverage-closure fix (TID-76) and the rest of the same
list.

Read with the same care as the cold numbers:

- **The warm path is 13× and 48× pytest's full run, and most of what is left is a bug.** The
  55 and 41 "tests" that run with nothing edited are exactly the candidates the projects' own
  `addopts` deselects or ignores — they produce no result (the tally does not add up by precisely
  their count) but they force a wellspring launch on every warm run. That is
  [TID-73](https://linear.app/snoodleboot/issue/TID-73). With it fixed the no-change run is the
  hash pass alone: measured after the fix, **0.14s** on pirn-core and **0.13s** on pirn-agents,
  `0 ran`, every result served from cache. Its sibling for parametrized tests ([TID-71](https://linear.app/snoodleboot/issue/TID-71))
  was found and fixed by the same benchmark the day before these numbers were taken.
- **Impact selection is right-sized.** A leaf edit re-runs its four dependents; a hub edit that
  3,953 tests depend on re-runs 3,843 of them and takes a third of the cold run. Selection is by
  recorded footprint, not by guess, and the hub case degrades to a large run rather than pretending.
- **The stale-pass check passes.** A leaf module made to raise on import produces failures in the
  next run on both suites — the check [TID-40](https://linear.app/snoodleboot/issue/TID-40) was
  filed for. The same scenario on fx_corpus found [TID-72](https://linear.app/snoodleboot/issue/TID-72):
  a *conftest* that raised was letting 508 of 511 tests pass without it.
- **On the second run, tiderace beats xdist on the suite it lost.** `warm_vs_xdist.py` on
  pirn-agents: after one run has recorded every test's duration, the work units are ordered by
  cost, and interleaved against `pytest -n auto` at the same load the medians are **xdist 55.2s,
  tiderace 35.5s — 1.56×** (the cold, count-ordered first run is 45.4s). Of the 35.5s, about 4.2s
  is the run's fixed start-up — importing every test module once, before any worker exists — and
  against a perfect-balance floor of 31.9s the second run sits at 1.11×. The start-up was the next
  lever, and [TID-75](https://linear.app/snoodleboot/issue/TID-75) took it: a run now imports only
  the test modules it executes (every conftest still, as pytest does). A one-dependent edit went
  from **3.6s to 1.4s on pirn-core** and 2.2s to 1.9s on pirn-agents — the latter's floor is the
  project's own import graph, which importing even one test module pulls in, and which pytest pays
  too.
- **Coverage capture cost a third of the cold run, and the reason was not coverage.** The daemon
  records every test's dependency footprint, and with capture on a cold pirn-core run was **45.8s
  against 34.1s** without it (+34%, per-test CPU 1.62×). Splitting that on the same binary, none of
  the suspects moved it: `LINE` events and `PY_START` events cost the same, sending file names
  without line numbers cost the same, instrumenting once per process instead of toggling
  `sys.monitoring` around every test cost the same. What every capture arm did and the "off" arm
  skipped was the static import closure (the TID-40 half of the footprint): each test module's
  closure re-parsed and re-resolved ~100 files, 230ms per module, 575 modules, once per worker.
  Skipping only that put capture-on level with capture-off. [TID-76](https://linear.app/snoodleboot/issue/TID-76)
  memoises the per-file parse and resolution and parses only the import statements (exact, with a
  full parse for anything that sits inside a string): all 537 closures take 0.46s instead of 73s,
  and the cold run with capture on is **34.1s against 31.9s** (+7%; 0 failures, 5,602 nodes). The
  same ticket found that `sys.monitoring.DISABLE` outlives the tool id, so on the in-process path
  only the first test in a worker to enter a function was ever credited with its file: 147
  pirn-core tests were missing dependencies, `test_agent_loop`'s up to 43 files each, and one
  parametrized case its own file. Diffing per-node footprints between the old and new shim shows
  the new one a strict superset on every one of the 147.
- **Where the warm run's time goes, drawn rather than guessed.** `--report` now records each
  node's worker, unit and unit start/end, and `timeline.py` draws the lanes
  ([TID-78](https://linear.app/snoodleboot/issue/TID-78)). pirn-core warm, 26.0s: seven of eight
  workers drain the queue and finish within 0.1s of each other at 19.1s; the eighth runs
  `test_loop_sub_tapestry.py::…::test_runs_far_past_the_old_recursion_ceiling` alone for 22.9s from
  t=0; the ideal makespan from the same spans is 22.9s. The wall is that test plus ~3s of start-up
  and teardown, and the overhead inside units is 3.0s of 157s. The cold run is 3s above its ideal
  only because, without durations, that unit was the 61st pick. So the floor on this suite is
  *start-up + the longest test*, the second run sits on it, and the levers that remain are the
  start-up and the tests themselves — the same floor `pytest -n auto` has, plus its own start-up.
  (One thing to keep an eye on: warm test time is 154s against 139s cold; the 8–23s "identity" tests
  loop until an address is reused and run slower on the bare tier. A test property, not a scheduler
  one.)
- **A file runs top to bottom in one process now.** The runs above were taken with module
  sharding on: a module heavier than one worker's share was split across workers, and the
  collector ordered tests alphabetically. Neither is pytest's behaviour, and a file whose tests
  build on each other's state failed here and passed there ([TID-80](https://linear.app/snoodleboot/issue/TID-80),
  found through a moto suite). Both are fixed — file order, one process per file — and sharding is
  opt-in (`--shard-modules`). It costs something on pirn-core: the one module above the cap holds the
  22.9s test and 18 others, and running it whole makes the critical path 25.7s instead of 22.9s —
  warm **26.0s → 28.8s** (single runs, not load-gated). `--shard-modules` buys that back for a suite
  whose files are known independent; a single-file suite now runs on one worker unless it asks.
  [TID-81](https://linear.app/snoodleboot/issue/TID-81) then moved the in-process restore from
  after every test to the module boundary, so a file's own globals accumulate across its tests as
  they do under pytest; per-test verdicts are unchanged. Parity after both: pirn-core 5,036 / 0
  with no outcome differences against the run before, and pirn-agents **4,652 / 0** — the one
  order-dependent test that had been the benchmark's single divergence now runs, in file order,
  before the import that broke it.
- **A cached failure stays failed.** pirn-agents' warm runs report `2 failing` from cache: the
  order-dependent test the cold benchmark names, plus one more under the daemon's scheduling. A
  cached verdict is served until its dependencies change, which is the contract.

## Reproduce

```bash
cargo build --release --manifest-path engine/Cargo.toml        # the engine

# The real-suite harness: one venv per corpus (see benchmarks/harness/README.md);
# PIRN_SNAPSHOT points at the internal monorepo snapshot for the four internal suites.
python benchmarks/harness/parity.py                             # pytest vs tiderace tallies, every corpus
python benchmarks/harness/nodediff.py click                     # per-node outcome diff — the only sound comparison
ROUNDS=3 python benchmarks/harness/timing_rr.py                 # pytest / xdist / tiderace, interleaved, medians
PIRN_SNAPSHOT=… python benchmarks/harness/second_run.py pirn-core   # the run after an edit
PIRN_SNAPSHOT=… python benchmarks/harness/warm_vs_xdist.py pirn-core

# The fixture microbenchmark
benchmarks/bench_3way.sh
```

`TIDERACE_BIN`, `TIDERACE_SHIM` and `TIDERACE_PY_TIDERACE` point a pass at a branch's binary and
shim, so two builds can be compared side by side. The method — pinned inputs, parity before speed,
interleaved rounds, load recorded, node-id comparison — is in
[`benchmarks/harness/README.md`](https://github.com/snoodleboot-io/tiderace/blob/main/benchmarks/harness/README.md);
the fixture microbenchmark's history is in
[`benchmarks/RESULTS-3way.md`](https://github.com/snoodleboot-io/tiderace/blob/main/benchmarks/RESULTS-3way.md)
and [`benchmarks/RESULTS-inproc.md`](https://github.com/snoodleboot-io/tiderace/blob/main/benchmarks/RESULTS-inproc.md)
(the in-process / FFI transport experiment, which confirmed the fork — not the pipe — was the cost).
