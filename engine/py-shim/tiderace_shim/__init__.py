"""The tiderace shim as a package (TID-116, option B).

`_shim.py` is the execution substrate — the only Python the engine ships — and `main()` in it is
the argv dispatch (`--probe`, `--subinterp`, else serve). The engine launches it through the
entry file beside this package (`py-shim/shim.py`; `tiderace/_shim/shim.py` once installed), so
`TIDERACE_SHIM` and `engine_core::default_shim` keep pointing at one file.

The foundations the rest builds on (TID-121): `results.py` (the result frames, spelled once),
`nodes.py` (what a node id names, resolved once into a `Target`; the module-name rule) and
`config.py` (the project's pytest configuration, loaded once), and `protocol.py` (the frames,
the one request loop, and the children the shim forks — TID-122), `pytest_compat.py` (both mark
dialects as one `Mark`, folded once; the marker API `request.node` and a hook item share),
`isolation.py` (what an in-process test may have disturbed, measured and put back — the
snapshots, verdicts and restores behind the no-fork tiers) and `safe.py` (attribute access that
treats any exception as absent), `plan.py` (what a node's run will execute, decided before
anything is set up) and `tiers.py` (the isolation ladder's tiers: the one place the tier is chosen,
and the node's response assembled from its variants) — TID-123. Phase 6e (TID-124) retires the
shim's globals: `config.py`'s `RunConfig` is the run (root, project, ignores, the `--modules`
set), `selection.py` what it selects (`-k`, `-m`, `--strict-markers`, the declared marks), both
on the engine; `log.py` is the one line to stderr.
"""
