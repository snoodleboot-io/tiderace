"""Bundled Python shim for the tiderace engine.

`shim.py` (the entry file) and `tiderace_shim/` (the package it hands over to) are staged into this
directory at wheel-build time from the canonical source (`engine/py-shim/`) — see
`scripts/build-wheel.sh`. The installed `tiderace` / `tiderace-daemon` binaries locate the entry
here automatically (engine_core::default_shim), so a `pip install tiderace` needs no
`TIDERACE_SHIM`. In a source checkout both are absent (git-ignored); dev/tests use the canonical
path, `engine/py-shim/shim.py`.
"""
