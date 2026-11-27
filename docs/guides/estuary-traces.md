# Estuary traces (`--etf`)

```bash
pip install 'tiderace[estuary]'          # the optional estuary-etf binding
tiderace run --etf run.etf tests/
```

`--etf <path>` writes, beside the usual report, one file in the **Estuary Trace Format** (ETF v0):
a record per test node, each with its outcome and a *trace* — every `(key, value)` the test read.
[estuary](https://github.com/snoodleboot-io/estuary) reuses a test's result on any snapshot where
every key in its trace still has the same value, so the trace has to be complete: a read it misses
is a stale pass waiting for the edit that exposes it.

Without the flag nothing here exists — no environment is set, no module is imported, no process is
spawned.

## What a trace holds

Keys are **file-granular**: `file:<path>`, relative to the repository root, `/`-separated. A value
is the BLAKE3 hash of the file's bytes (`estuary_etf.value_hash("file", …)`), or *absent* when the
key names nothing.

Traces overlap almost entirely — every test in a module reads the same imports, conftests and
toolchain — so a trace is a small tree of shared **fragments**, and a test's own record holds only
what is left:

```
root(test) ── toolchain          tool:python · tool:pyenv · tool:tiderace
           ├─ config             one key per config section the run reads
           ├─ conftests(dir)     the conftest chain, repository root → the test's directory
           ├─ imports(module)    the test module's import closure
           └─ own pairs          files the test executed code in that nothing above names
```

| Fragment | Keys | Shared by |
|---|---|---|
| **Toolchain** | `tool:python` (implementation, version, platform), `tool:pyenv` (every installed distribution, `name==version`), `tool:tiderace` (the harness version). Its id is in the file header. | every record of the run |
| **Config** | One `derived:cfg/<file>#<section>` per section tiderace reads — `[pytest]`, `[tool:pytest]`, `[tool.pytest.ini_options]`, `[tool.pytest]`, `[tool.tiderace]` — valued by the section's canonical JSON. A pytest section that is not there is valued as `null`: its presence is what makes a directory the rootdir. Every directory the loader looked in is keyed — the run path, then upward to the one whose config carries a pytest section — and a `pytest.ini`, `pyproject.toml`, `tox.ini` or `setup.cfg` that could exist there and does not is a `file:` key with no value. | every record of the run |
| **Conftest chain** | For each directory from the repository root down to the test's: its `conftest.py` and everything that conftest imports, or the key with no value where there is none. Each directory's fragment includes its parent's. | every test under the directory |
| **Import closure** | The test module, every file in the repository it imports, transitively, and the `__init__.py` of each package on the way. One fragment per file (per import cycle, where there is one), including the fragments of what it imports — so two test modules that import one package share that package's fragments. | every test whose module imports the file |
| **Own pairs** | Files the test ran code in (`sys.monitoring`, the same capture impact analysis uses) that none of the above names: a module imported inside the test, a plugin reached through a registry. | one record |

Editing `[project]` in `pyproject.toml` therefore changes no trace; editing
`[tool.pytest.ini_options]` changes all of them. Editing `tests/api/conftest.py`, or a helper it
imports, changes the traces of exactly the tests under `tests/api/`. Adding a `conftest.py` to a
directory that had none changes the traces beneath it, because the key was already there, naming
nothing.

### The import closure is the module's, not the first test's

A module's `import` lines execute once per process, for whichever test runs first. Coverage alone
credits the imported source to that one test, and every later test in the module is served a stale
result when the source changes (TID-40). The closure is built from the import graph instead, so
all twenty tests of a twenty-test module carry it. A virtualenv inside the checkout is not walked:
`tool:pyenv` covers what is installed.

## Check ids, outcomes, header

- **Check id** — the node id, repository-relative: `tests/test_rates.py::test_prorate[feb-leap]`.
  `tiderace run tests/` and `tiderace run .` write the same ids over the same source keys (the
  config keys follow where the loader actually looked, which starts at the run path);
  parametrized cases and inherited methods carry the ids the report shows.
- **Outcome** — `passed → pass`, `failed → fail`, `error → error`, `skipped → skip`. `xfail` and a
  non-strict `xpass` are `pass`: the run went the way the suite says is fine, as the exit code
  already treats them.
- **Header** — `harness = tiderace`, `harness_version` (the installed distribution's),
  `granularity = file`, an empty `u_version`, the toolchain fragment's id, `created_at`
  (`SOURCE_DATE_EPOCH` when set), and `tree`: the snapshot id from `TIDERACE_ETF_TREE` (64 hex
  digits) when the caller knows it, zeros otherwise.

`--etf-root <dir>` sets the directory keys are relative to. By default it is the nearest directory
at or above the run path that holds `.git`, else the run path itself.

## What a trace does not hold yet

A trace is exactly as sound as it is complete. These reads are **not** recorded today; each is
tracked, and the emitter is laid out so that each is one more source of fragments or one more kind
of key in a test's reads (`tiderace_shim/etf.py`, `SOURCES`):

| Missing | Consequence | Tracked |
|---|---|---|
| Import probes that found nothing (`sys.path` entries searched before the hit) | A new file that shadows an imported module does not invalidate | TID-129 |
| Environment variables read | A changed variable does not invalidate | TID-129 |
| Files read as data (`open`, `importlib.resources`, fixtures loading JSON) and directory listings | A changed data file does not invalidate unless Python code in it ran | TID-129 |
| Unit keys (`unit:<path>#<qualname>`) | Any edit to a file invalidates every test that reads the file, not only those that ran the edited function | TID-130 |
| The carry-over chain for tests run in one process (estuary design 03, D-306) | A value an earlier test cached in the process is not attributed to the later test that used it | TID-154 |
| Dynamic imports made *outside* the test — `importlib.import_module` at another module's import time, plugin entry points | Seen only when the imported file's code runs inside the test | TID-154 |
| Reads by subprocesses, C extensions and other threads' frames | Not seen | — |
| Determinism pins (`PYTHONHASHSEED`, seeds, `TZ`, locale) | tiderace applies none; `seed` is 0 and no pin keys are written | TID-154 |

Under `estuary-runner` the file-level reads in that table are covered from outside the process by
the filesystem floor, and the runner — not the harness — computes every value. Standalone, the
file is what tiderace alone can see: use it for measurement and development, not as a gate.

Values are hashed when the file is written, after the run. A file a test rewrote during the run is
recorded with its final content. Records are written for the nodes this run executed: `--etf` is
an option of `tiderace run`; the impact-aware `tiderace-daemon run`, and a run a daemon serves,
write none.

## Reading a file back

```python
import estuary_etf as etf

run = etf.decode(open("run.etf", "rb").read())      # raises etf.EtfError on a malformed file
for record in run.records:
    keys = run.flatten(record["root"])              # [(kind, name, value_or_None), …]
    print(record["check"].decode(), record["outcome"], len(keys), run.trace_digest(record["root"]).hex())
```

Two runs of an unchanged tree give every check the same trace digest; after an edit, the checks
whose digest changed are the ones the edit reaches.

## Cost

`--etf` turns footprint capture on — the capture `tiderace-daemon run` always has on — and after
the run parses the import graph once and hashes each file once. The measured cost is in the
[benchmarks guide](benchmarks.md#the-cost-of-etf).
