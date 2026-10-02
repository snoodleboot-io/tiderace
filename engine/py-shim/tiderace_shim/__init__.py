"""The tiderace shim as a package (TID-116, option B; the layout of TID-124).

The only Python the engine ships. The engine launches it through the entry file beside this
package (`py-shim/shim.py`; `tiderace/_shim/shim.py` once installed), so `TIDERACE_SHIM` and
`engine_core::default_shim` keep pointing at one file; `main()` in `modes.py` is the argv dispatch
(`--probe`, `--subinterp`, else serve).

Top to bottom — nothing imports upward, and `tests/test_layout.py` checks it:

- `modes.py` — what the shim does when launched: the worker loop, alone or as a pool forked from
  one imported image; the sub-interpreter probe and pool.
- `engine.py` — one `Engine` per worker process: `run()` is gate → plan → route → execute →
  assemble; the module child for an opaque module and the clean room for a demoted test.
- `plan.py` (what a node's run will execute: its cases and ids, decided before anything is set
  up), `tiers.py` (the isolation ladder's tiers, chosen once; the in-process deadline),
  `discovery.py` (what the suite defines: the registry and everything the walk learned).
- `invoke.py` (calling a test, written once for sync and async), `isolation.py` (what an
  in-process test may have disturbed, measured and put back), `footprint.py` (what a test
  touches: coverage, import closures, the process's memos), `fixtures.py` (the fixture model and
  the registry), `selection.py` (`-k`, `-m`, `--strict-markers`), `pytest_compat.py` (both mark
  dialects as one `Mark`; `request.node` and a hook item).
- `config.py` (the project's pytest configuration and the run's), `nodes.py` (what a node id
  names, resolved under the run root), `results.py` (the result frames, spelled once),
  `protocol.py` (the frames, the one request loop, the children the shim forks).
- `safe.py` (attribute access that treats any exception as absent), `log.py` (the one line to
  stderr).
"""
