# Benchmark harness — parity first, then wall clock

The scripts behind the *Tiderace on Eight Suites* benchmark document. `run_benchmarks.py` one
directory up measures a generated fixture under hyperfine; this directory measures **real suites**
— vendored public projects and a snapshot of the internal monorepo — and, before it times
anything, checks that tiderace agrees with pytest on them test for test.

| script | what it does |
| -- | -- |
| `corpora.py` | the corpus list; every other script imports it |
| `parity.py` | pytest vs tiderace tallies per corpus, from `tiderace run --report` |
| `nodediff.py` | per-node outcome diff for one corpus — the only sound comparison |
| `timing_rr.py` | pytest / `pytest -n auto` / tiderace, interleaved rounds, medians, load recorded |
| `binab.py` | two tiderace binaries A/B'd on the same corpora, interleaved |
| `analyse_bins.py` | rebuild the scheduler's bins from a report and charge them measured durations |
| `second_run.py` | the run after an edit: warm no-change, leaf edit, hub edit, injected failure (TID-65) |
| `warm_vs_xdist.py` | tiderace on its second run (duration-ordered) against `pytest -n auto` (TID-52) |
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
python benchmarks/harness/parity.py cachetools        # 215 passed on both sides
```

For the internal corpora, snapshot the monorepo at a commit (source plus a *copy* of its venv,
with the `.pth` entries rewritten to the copy) and point `PIRN_SNAPSHOT` at it. The snapshot stays
a faithful copy of the project's environment, so pytest-xdist — the benchmark's comparison, not the
project's dependency — is installed beside it and reached through `PYTHONPATH`:

```bash
uv pip install --python $PIRN_SNAPSHOT/.venv/bin/python --target .tiderace-bench-venvs/xdist pytest-xdist
```
