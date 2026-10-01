"""The tiderace shim as a package (TID-116, option B).

`_shim.py` is the execution substrate — the only Python the engine ships — and `main()` in it is
the argv dispatch (`--probe`, `--subinterp`, else serve). The engine launches it through the
entry file beside this package (`py-shim/shim.py`; `tiderace/_shim/shim.py` once installed), so
`TIDERACE_SHIM` and `engine_core::default_shim` keep pointing at one file. Phases 6b–6e split
`_shim.py` into modules here.
"""
