"""Bundled Python shim for the tiderace engine.

`shim.py` is staged into this directory at wheel-build time from the canonical source
(`engine/py-shim/shim.py`) — see `scripts/build-wheel.sh`. The installed `tiderace` / `tiderace-daemon`
binaries locate it here automatically (engine_core::default_shim), so a `pip install tiderace` needs no
`TIDERACE_SHIM`. In a source checkout it is absent (git-ignored); dev/tests use the canonical path.
"""
