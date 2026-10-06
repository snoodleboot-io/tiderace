# Benchmarks

Every number here comes from a harness in the repository; run it rather than trust it. Results
vary by machine and, above all, by how fast your Python imports your suite's dependencies.

## The short version

Eight real suites — four public projects vendored at fixed commits, four internal ones from a
pinned monorepo snapshot — run by pytest and by tiderace in the same virtualenv, compared **test
by test** before anything is timed. As of the 5 October pass on main at `a178434`:

| | |
| -- | -- |
| parity | identical outcomes on seven of eight suites, node id for node id; the eighth (anyio) collects the same 1,479 nodes and fails none that pytest passes |
| a full run, against serial pytest | 2.0× to 4.9× faster on seven suites |
| a full run, against `pytest -n auto` | 1.5× on the two 5,000-test suites; more on small suites, where xdist's start-up is the whole run |
| a warm run with nothing edited | 0.19s and 0.17s on the two 5,000-test suites, against pytest's 77s and 104s and pytest-testmon's 1.9s and 2.5s |
| one leaf module edited | 1.9s and 3.4s (testmon: 2.0s and 2.7s); the hub module, 24s and 15s (testmon: 61s and 17s) |
| one test by name through a warm daemon | 0.3s on a 5,600-test suite (4s without the daemon) |

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
python -m benchmarks.harness.parity             # pytest vs tiderace tallies, per corpus
python -m benchmarks.harness.nodediff click     # per-node outcome diff — the only sound comparison
python -m benchmarks.harness.selection_diff click -k context "not shell"   # what -k / -m select, vs pytest
python -m benchmarks.harness.timing_rr          # pytest / xdist / tiderace, interleaved, medians
PIRN_SNAPSHOT=... python -m benchmarks.harness.second_run pirn-core   # the run after an edit
```

The method — pinned inputs, parity before speed, interleaved rounds, load recorded, node-id
comparison — is in that directory's README. `second_run.py` is the benchmark the cold numbers above
do not cover: a warm run with nothing edited, one edit to a leaf module, one to a hub module, and an
edit that must produce a failure (the stale-pass check). Its numbers are in the section below.

### The eight-suite pass, 5 October 2026

The same pass on main at `a178434`, after the code-structure redesign (TID-116 … TID-124, TID-110)
and the fix of the one regression it introduced (TID-125): the in-process tier had taken the idle
worker-thread pools that anyio, trio and asyncio keep alive for leaked threads, and sent 144 of
anyio's 1,479 nodes to a forked re-run — anyio's full run had gone from 9.6 s to over 20 s. The
pass that found it (2 October) was run on a loaded machine and is not published; this one waited
for a quiet one — one-minute load 1–3 at the start of every timed chunk, 1–8 during, recorded with
every sample. Three interleaved rounds after a discarded warm-up, medians. Since 1 October a
timing gate runs on every shim or engine change (`shimab`, TID-126), so the next such regression
is caught in the pull request, not in a monthly pass.

**Parity** is the 1 October result unchanged: eight suites, 0 outcome differences on seven, anyio
collects the same 1,479 node ids and fails none that pytest passes; its divergence is pytest's own
(32 errors on pytest's side that are its `pytester` fixtures running under a different plugin
set).

**Cold timings**, median wall clock in seconds; 1 October in brackets where it moved more than
the noise.

| suite | pytest | pytest -n auto | tiderace | vs pytest | vs xdist |
| -- | --: | --: | --: | --: | --: |
| pirn-core | 75.6 | 37.7 | **25.8** [27.2] | 2.93× | 1.46× |
| pirn-agents | 104.2 | 45.5 | **31.8** | 3.28× | 1.43× |
| pirn-data | 32.1 | exit 3 | **15.1** | 2.12× | — |
| anyio | 48.7 | 14.7 | **9.9** | 4.92× | 1.49× (partial parity) |
| cachetools | 0.71 | 1.77 | **0.51** | 1.40× | 3.5× |
| click | 1.66 [1.38] | 2.43 [1.92] | **0.74** | 2.23× | 3.3× |
| flask | 2.58 [2.09] | 3.04 [2.54] | **1.28** | 2.02× | 2.4× |
| fx_corpus | 0.93 | 2.36 | 0.95 | 0.98× | 2.5× |

The runners are where they were: every tiderace number is within 5% of 1 October except
pirn-core's, which is 5% better. click and flask read 20% slower for pytest and xdist alike and
7% and 5% for tiderace — a change every tool shares is the machine's (these suites run in one or
two seconds, where a page cache and a scheduler decide), and `bench_diff` is what says so. One
pirn-agents tiderace round took 62 s against 31.5–31.8 s for the other three; the median
discards it. The same stall — one deadline's worth, on a run that otherwise passes — has now been
seen on anyio too (in about one full run in thirty), and the pass's own data says what it is:
in the stalled runs one socket test's duration is the deadline and nothing else is slow.
`test_happy_eyeballs` starts a listener on one address family and connects to `localhost`, which
on this machine resolves to 127.0.0.1 only, so six of its nine variants fail — under pytest too —
and leave their `accept()` thread behind, so tiderace re-runs them from the clean image. With
eight workers running socket tests at once, the connect to the *wrong* family occasionally lands
on another worker's listener that happens to hold the same ephemeral port, succeeds, and the
test's own `thread.join()` then waits for an `accept()` that never comes: the deadline ends it.
Serial pytest never has another listener to hit. A parallel runner's artefact, paid as one
deadline, not a correctness difference (the outcomes match pytest's); since the pass the
clean-room warning names the disturbance and how the in-process attempt ended, which is what
made this traceable (TID-127).

**Memory**, peak proportional set size of each runner's whole process tree in MB, sampled every
200 ms, the highest of the three rounds.

| suite | pytest | pytest -n auto | tiderace |
| -- | --: | --: | --: |
| pirn-data | 812 | 1,606 (exit 3) | 6,300 |
| pirn-agents | 1,173 | 2,777 | 3,271 |
| pirn-core | 459 | 2,018 | 2,440 |
| anyio | 222 | 481 | **596** |
| flask | 57 | 365 | **179** |
| click | 41 | 275 | **93** |
| fx_corpus | 49 | 316 | **57** |
| cachetools | 39 | 257 | 371 |

The monorepo suites hold where they were (pirn-data's 6.3 GB is still eight Spark JVMs). Two
rows looked like changes and were checked by sampling *who* holds the memory at the peak (the
pool's eight workers, their clean-room forks, the children tests spawn). anyio's tiderace peak
read 292 MB on 1 October and 596 here: the eight pool workers hold 235 MB with the 1 October
shim, 270 after TID-124 step 3 and 320 with the current one — about 10 MB more per worker, which
is the in-process tier's isolation bookkeeping — and the rest of the 596 is one 200 ms sample
catching forks and spawned children together (the peak reads 374–462 MB on three more runs).
pirn-core's three rounds read 2,440, 1,406 and 1,382 MB at the same wall clock: the pool holds
1.2 GB and the clean-room forks 0.13, so the two-gigabyte samples are the children its tests
spawn (dispatcher and pool tests) coinciding, not a first-run effect — three further runs from a
clean state read 1,444, 1,791 and 2,766 MB in that order. The small suites' peaks are one
sample wide too: cachetools' three rounds read 371, 47 and 142 MB.

**The second run**, with pytest-testmon as the second comparison. testmon is the closest thing
pytest has to a second run — coverage recorded on the first `--testmon` run, affected tests
selected by fingerprint on the next — and it is run here the way its own documentation says to,
with `--testmon-forceselect` because these suites deselect their slow marks in `addopts`, and
coverage's C tracer because its default on 3.14 records no contexts. The edit is one appended
statement, not a comment (a comment is invisible to testmon's fingerprints). Medians of three.

| scenario | pirn-core: pytest · testmon · tiderace | pirn-agents: pytest · testmon · tiderace |
| -- | -- | -- |
| cold, full run (the one that records) | 76.7 · 116.5 · **25.9** s | 104.0 · 169.4 · **39.8** s |
| warm, **nothing edited** | 76.7 · 1.87 · **0.19** s | 104.0 · 2.51 · **0.17** s |
| one leaf module edited (4 / 1 dependents) | 76.7 · 2.00 (2 ran) · **1.91** (4 ran) s | 104.0 · 2.69 (1 ran) · **3.42** (1 ran) s |
| the hub module edited (3,958 / 2,428 dependents) | 76.7 · 60.8 (816 ran) · **23.9** (3,793 ran) s | 104.0 · 16.6 (499 ran) · **14.8** (2,316 ran) s |
| leaf module made to raise on import | reported · reported · **reported** | reported · reported · **reported** |

(On pirn-agents the leaf module is imported while pytest loads the suite's conftest, so testmon's
run stops there with the traceback and exit 4 — reported, before a test runs.)

The two tools choose differently and pay differently. testmon selects by coverage, down to the
code block, so on the hub edit it runs 816 tests where tiderace's file-level footprints run
3,793; then it runs them serially under the tracer, so it takes 2.5× as long on pirn-core and
12% longer on pirn-agents, and its recording run costs 1.5–1.6× a plain pytest run where
tiderace's costs a third of one. On a leaf edit and on nothing-edited the two are within a second
and a half of each other, and either is 30–600× a full run. tiderace's own rows are 1 October's
within a tenth of a second, the hub edit 2 s faster on pirn-core.

**Warm vs xdist** (`warm_vs_xdist.py`, four interleaved rounds after the priming run): pirn-core
**xdist 37.4 s, tiderace warm 24.5 s, 1.52×**; pirn-agents **44.8 s against 30.5 s, 1.47×**.
1 October: 1.55× and 1.46×.

**Worker scaling**, pirn-core cold, median of two: `--workers 1` 83.2 s, `2` 43.9 s, `4` 27.3 s,
`8` 24.5 s. 1 October: 83.2 / 45.0 / 28.6 / 27.7.

**Suite size**, the synthetic suites again, median of three.

| run | 2,000 tests · 40 modules | 20,000 tests · 400 modules |
| -- | --: | --: |
| pytest | 1.85 s | 16.2 s |
| pytest -n auto | 2.94 s | 18.7 s |
| tiderace, cold | **0.63 s** | **4.11 s** |
| tiderace, warm daemon | 0.27 s | 5.26 s |
| pytest -k one test | 0.66 s | 4.49 s |
| tiderace -k one test, no daemon | 0.39 s | 1.20 s |
| tiderace -k one test, warm daemon | **0.09 s** | **0.85 s** |

1 October's table to within 0.1 s on every row.

**The isolation ladder's cost per test** (`ladder_bench.py`, new with TID-126): 1,000 trivial
tests, sync and `async def`, cold, no daemon, median of three, in microseconds per test.

| tier | sync | async |
| -- | --: | --: |
| the ladder (default: in-process with snapshot/restore, 8 workers) | **494** | **638** |
| `--strategy subprocess` (in-process, no fork anywhere) | 772 | 1,093 |
| `--no-optimistic` (a fork per test) | 1,319 | 2,102 |
| the ladder on one worker | 994 | 1,564 |

An async test on the ladder costs 1.3× a sync one — the event loop and the async fixture. Before
TID-125 it cost 5× (3,219 µs on a loaded machine, 933 after the fix on the same machine); this
quiet-machine pass is the baseline the gate compares against from here.

### The eight-suite pass, 1 October 2026

`bench_pass.sh` again on main at `794153e` (after TID-96 … TID-103): parity, node diff, cold
timings with peak memory, the second run, warm vs xdist, worker scaling. Same machine, one-minute
load 3–11 recorded with every sample; three interleaved rounds after a discarded warm-up, medians.
Parity is the 29 September result unchanged: eight suites, 0 outcome differences on seven, anyio's
8 tiderace-only failures all `pytester` — and 0 since TID-105 provided `pytester` and `testdir`
(1 October, after the pass): anyio collects the same 1,479 node ids as pytest and fails none of
them that pytest passes.

**Cold timings**, median wall clock in seconds.

| suite | pytest | pytest -n auto | tiderace | vs pytest | vs xdist |
| -- | --: | --: | --: | --: | --: |
| pirn-core | 78.0 | 39.9 | **27.2** | 2.86× | 1.47× |
| pirn-agents | 104.7 | 47.9 | **32.4** | 3.23× | 1.48× |
| pirn-data | 30.6 | exit 3 | **14.6** | 2.09× | — |
| anyio | 48.4 | 15.0 | **9.6** | 5.06× | 1.57× (partial parity) |
| cachetools | 0.70 | 1.64 | **0.47** | 1.49× | 3.5× |
| click | 1.38 | 1.92 | **0.69** | 2.0× | 2.8× |
| flask | 2.09 | 2.54 | **1.21** | 1.73× | 2.1× |
| fx_corpus | 0.92 | 2.21 | 0.93 (**0.56** with `--shard-modules`) | 0.99× | 2.4× |

`warm_vs_xdist.py`, the dedicated head-to-head at the same load: pirn-agents **xdist 47.0 s,
tiderace warm 32.1 s, 1.46×**; pirn-core **40.9 s against 26.4 s, 1.55×**.

**Memory**, peak proportional set size of each runner's whole process tree in MB, sampled every
200 ms, one run each. PSS charges a page the fork pool's workers share with their image once,
divided among them; summed RSS charged it to every process that maps it and read nine gigabytes
for pirn-data where there are 6.5.

| suite | pytest | pytest -n auto | tiderace |
| -- | --: | --: | --: |
| pirn-data | 831 | 1,436 (exit 3) | 6,516 |
| pirn-agents | 1,099 | 2,721 | 3,863 |
| pirn-core | 470 | 1,930 | 2,426 |
| anyio | 213 | 455 | **292** |
| flask | 61 | 358 | **179** |
| click | 53 | 279 | **69** |
| fx_corpus | 49 | 313 | **79** |
| cachetools | 39 | 268 | **45** |

On the small suites tiderace holds well under xdist — eight forked workers share the imported
image where xdist's eight are eight interpreters. On the monorepo suites it holds more: 2.4 GB
against xdist's 1.9 on pirn-core, 3.9 against 2.7 on pirn-agents, and 6.5 GB on pirn-data, whose
tests start a Spark JVM in each worker that touches them — eight JVMs where serial pytest starts
one. The suite's shape, not a leak; the number to know before running it on a small box.

Since TID-106 the number is knowable and the default accounts for it: the default worker count
is capped by what memory allows once the imported image is up (a share of what is available,
less the image, at half the image's size per worker — the growth measured above), a count given
with `--workers` is used as given, `--memory-limit <MB>` (or `TIDERACE_MEMORY_LIMIT_MB`, which the
daemon reads too) caps the pool whatever the count, and every run reports each worker's peak
resident size — on the terminal as one line and in `--report` as a `workers` table. Linux only:
that is where the kernel reports both numbers.

**Worker scaling**, pirn-core cold, median of two rounds: `--workers 1` 83.2 s, `2` 45.0 s, `4`
28.6 s, `8` 27.7 s — linear to the four physical cores, flat across the hyper-threads. Serial
tiderace at 83 s is pytest's 78 s plus the restore bookkeeping; the parallelism is the whole of
the win on a full run.

**Suite size.** Synthetic suites from `scale_corpus.py` — fifty trivial tests a module, one
parametrized and one marked per module, a session fixture half the tests take — so the tests cost
nothing and the runner's own cost is what grows. Median of three.

| run | 2,000 tests · 40 modules | 20,000 tests · 400 modules |
| -- | --: | --: |
| pytest | 1.87 s | 16.4 s |
| pytest -n auto | 3.02 s | 19.6 s |
| tiderace, cold | **0.71 s** | **4.24 s** |
| tiderace, warm daemon | 0.26 s | 5.51 s |
| pytest -k one test | 0.65 s | 4.43 s |
| tiderace -k one test, no daemon | 0.43 s | 1.22 s |
| tiderace -k one test, warm daemon | **0.10 s** | **0.81 s** |

The cold run scales as it should (2.6× pytest, then 3.9×) and xdist is slower than serial pytest
at both sizes on tests that cost nothing. One test by name through the daemon is 6.5× and 5.5×
faster than pytest's own `-k`. Two honest readings: the daemon's *full* run on 20,000 trivial
tests is slower than a cold run (5.5 s against 4.2) — with nothing to import, the image saves
nothing and the bookkeeping of 20,000 records (footprints, keywords, durations) is what remains;
the daemon is for suites with an import graph, and for `-k`. And `-k` through the daemon grows
from 0.10 s to 0.81 s across the tenfold, most of it the state file's load and the hash of every
known file, which grow with the suite where TID-102's decision itself stays small.

**Windows and macOS** (TID-13), by the `bench-platforms` workflow on hosted runners: a release
build, Python 3.14.7, the corpora that need no private snapshot, three rounds after a warm-up.

| platform · suite | pytest | pytest -n auto | tiderace | `--workers 1` | `--strategy subinterp` |
| -- | --: | --: | --: | --: | --: |
| windows-latest · fx_corpus | 0.66 | 1.37 | 0.80 | 0.66 | 0.90, exit 1 |
| windows-latest · cachetools | 0.47 | 1.10 | 0.49 | 0.42 | 0.56, exit 1 |
| macos-14 · fx_corpus | 0.42 | 0.79 | 0.52 | 0.53 | — |
| macos-14 · cachetools | 0.29 | 0.61 | 0.23 | 0.30 | — |

On suites this small every parallel tier pays more in start-up than it earns, on every platform,
xdist most of all. Windows has no fork, so tiderace's default there is the no-fork subprocess
worker; the sub-interpreter tier ran both suites but exited 1 on each — tests pass in the
subprocess tier that failed in a sub-interpreter — and hung for an hour on click's suite. Both
were one defect, fixed since (TID-104): the tier read each reply as a single outcome, so a `-k`
deselection came back a pass and a parametrized node one result; and nothing ended a test that
blocked there. The tier now reads whole replies, its shim parent waits at most the deadline for
each and names what is outstanding, the engine kills a pool whose reply is overdue, and modules
that set process-wide state (the cwd, the environment, signals — shared by every sub-interpreter
in the process) are routed to the fallback by the probe. click's suite in the tier on Linux is
pytest's answer, 589 passed and 21 skipped, in about 1.5 s; the next workflow run remeasures
Windows. It is still not a tier to select by default. click 8.1.7's own suite does not collect under
pytest on 3.14, so it is not a baseline on these runners. The monorepo suites, where the parallel
tiers earn their keep, have no Windows measurement: their snapshots are private and the machine
is Linux.

**The second run**, 1 October (29 September in brackets): pirn-core — pytest 78.1 s, cold
`run --all` 27.4 s [30.7], nothing edited **0.18 s** [0.16], a leaf edited **1.93 s** [1.38], the
hub edited 25.7 s [25.9], a failing edit reported in 1.70 s [1.09]; pirn-agents — pytest 105.9 s,
cold 41.1 s [40.3], nothing edited **0.17 s** [0.13], a leaf **3.42 s** [1.79], the hub 15.5 s
[13.9], a failing edit 2.72 s [1.49]. The leaf and failing-edit rows are slower than in September
by a second or two: the daemon now records keywords for every node and re-baselines the hashes of
more files (TID-102, TID-101), and both are paid on the run after an edit.

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
| pirn-core | 5,036 | 0 | 522 tests in 47 modules that skip at import (`pytest.importorskip`): pytest counts each module as one skip, tiderace reports every test in it — 91 skipped against 566, the same 47 modules. The 32 tests the project's `addopts -m` deselects are absent on both sides |
| pirn-agents | 4,652 | 0 | 0 |
| cachetools | 215 | 0 | 0 |
| click | 589 + 1 xfail + 21 skipped | 0 | 0 |
| flask | 475 + 4 failed + 3 error + 2 skipped (both sides) | 0 | 0 |
| anyio | 1,479 | 0 — the last 8 were `pytester`'s `testdir`, provided since TID-105 | 0 |

The click and flask "skipped" rows read as tiderace-only until [TID-88](https://linear.app/snoodleboot/issue/TID-88):
`nodediff.py` took pytest's side from `-rA`, whose summary folds every skip into a `SKIPPED [16]
file:line` line with no node id, so pytest's own skipped variants were invisible to the diff. It now
reads `-v`, one line per node. anyio went from 222 failed/error and 332 / 384 ids only one side has
(TID-86) to 0 / 0 ids and 31 failed/error after TID-87 (plugin fixtures) and TID-88 (a parametrized
fixture name not in the signature is indirect; duplicate ids take pytest 8's `_` suffix; a skip-marked
parametrized test is skipped per variant; a fixture named `test*` is not a test).

**Selection parity.** A runner that agrees with pytest on outcomes but not on *what `-k` and `-m`
select* is still a different runner. Measured 30 September, after TID-100, as sets of node ids —
tiderace's `--report` against pytest's `--collect-only` for the same expression, in the same venv:

| suite (pytest) | expressions | result |
| -- | -- | -- |
| pirn-core (9.1) | `-k unit`, `end_to_end`, `connectors` — directory names | 4,557 / 38 / 2,337, pytest's exactly (plus the 522 module-import skips tiderace reports either way) |
| click (7.4) | `-k context`, `utils and not echo`, `not shell`, `tests` | identical sets, 611 for `tests` |
| anyio (9.1) | `-k socket`, `asyncio and not trio`, `streams`, `tls and asyncio` | identical sets |
| pirn-core, anyio, pirn-agents | ten `-m` expressions (`slow`, `not slow`, `anyio`, `needs_postgres or needs_kafka`, `heavy`, `network`, `not network`, …) | identical sets |

Directory names were the gap: pytest 8 puts every directory below the rootdir on a node's chain,
so `-k unit` selected 4,557 pirn-core tests under pytest and none under tiderace until TID-100;
pytest 7 names a module by its whole path from the rootdir unless its own directory is a package,
and the shim follows whichever pytest the venv has.

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

### The warm daemon, measured

`tiderace daemon start` keeps the imported suite as an image and forks each run's workers from it
(TID-84); a `-k` run travels to it with its selection (TID-90). Measured 30 September on main at
`27d7abe`, after TID-96 — one-minute load 8–10, which is the run itself on an 8-thread machine;
three runs where three numbers are shown, otherwise one.

| run | pirn-core (5,602 nodes) | pirn-agents (4,657 nodes) |
| -- | --: | --: |
| `tiderace run`, no daemon | 26.3 / 26.6 s | 31.8 / 31.8 s |
| `tiderace run -k <one test>`, no daemon | 4.1 s | 3.9 s |
| daemon, first run (the image import) | 26.2 s | 31.4 s |
| daemon, warm, unfiltered | **23.0 / 23.5 / 23.0 s** | **28.4 / 28.5 / 28.2 s** |
| daemon, `-k <one test>` | **0.64 / 0.62 / 0.63 s** | **0.42 / 0.43 / 0.40 s** |
| daemon, `-k` matching nothing | 0.63 / 0.60 / 0.59 s | 0.39 / 0.39 / 0.39 s |
| daemon, `-k <one test>`, after TID-102 | **0.33 / 0.33 / 0.31 s** | **0.26 / 0.29 / 0.33 s** |
| daemon, `-k` matching nothing, after TID-102 | 0.31 / 0.32 / 0.30 s | 0.21 / 0.24 / 0.22 s |
| daemon, after a source edit (re-import) | 26.5 s | 32.1 s |
| daemon, warm again | 22.7 s | 28.6 s |

What the image buys is the start-up — the interpreter and the suite's import graph, ~3.5 s here —
so a full run through the daemon is 12% faster and a run of one test by name is **6× to 9×**
faster than without it. TID-102 (1 October) then halved the `-k` round trip again: the daemon
decides `-k` itself for every node whose recorded keywords and dependencies are unchanged, and
replays a module-import skip from its record instead of sending the node to a worker to import
and skip it again; one test by name is **12× to 15×** faster than without the daemon. An edit under the tree drops the image and the next run pays the import
once; the impact-aware `tiderace-daemon run` above, which imports only what it will execute, is
still the cheaper inner loop after a source edit.

pirn-agents' 31.8 s local run is 0.7 s slower than the pass's 31.1 s, and honest where that one
was not: the cross-process replay class in that suite paid its 5 s `setUpClass` once per method
under tiderace — a recorded state-disturber was forked per test — and the pass's number was
taken before the disturber records existed (TID-96). The class alone was 38 s against pytest's
6.8 s; it is 7.9 s now.

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
python -m benchmarks.harness.parity                             # pytest vs tiderace tallies, every corpus
python -m benchmarks.harness.nodediff click                     # per-node outcome diff — the only sound comparison
ROUNDS=3 python -m benchmarks.harness.timing_rr                 # pytest / xdist / tiderace, interleaved, medians
PIRN_SNAPSHOT=… python -m benchmarks.harness.second_run pirn-core   # the run after an edit
PIRN_SNAPSHOT=… python -m benchmarks.harness.warm_vs_xdist pirn-core

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
