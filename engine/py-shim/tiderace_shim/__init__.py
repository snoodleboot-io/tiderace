"""The tiderace shim as a package (TID-116, option B).

`_shim.py` is the execution substrate — the only Python the engine ships — and `main()` in it is
the argv dispatch (`--probe`, `--subinterp`, else serve). The engine launches it through the
entry file beside this package (`py-shim/shim.py`; `tiderace/_shim/shim.py` once installed), so
`TIDERACE_SHIM` and `engine_core::default_shim` keep pointing at one file.

The foundations the rest builds on (TID-121): `results.py` (the result frames, spelled once),
`nodes.py` (what a node id names, resolved once into a `Target`; the module-name rule) and
`config.py` (the project's pytest configuration, loaded once). Phases 6c–6e carry on splitting
`_shim.py` by concern.
"""
