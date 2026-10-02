#!/usr/bin/env python3
"""The shim's entry file (TID-116, option B).

The shim is the package beside this file, `tiderace_shim/`; this file is what the engine launches
(`python <shim> <root> [--probe|--subinterp]`), what `TIDERACE_SHIM` points at, and what
`engine_core::default_shim` finds staged as `tiderace/_shim/shim.py` in an installed wheel. It
puts its own directory on `sys.path` so the package resolves from either place, then hands over.

The proof scripts and the unit tests import the package's modules directly.
"""
from __future__ import annotations

import os
import sys

_HERE = os.path.dirname(os.path.abspath(__file__))
if _HERE not in sys.path:
    sys.path.insert(0, _HERE)

from tiderace_shim.modes import main  # noqa: E402

if __name__ == "__main__":
    sys.exit(main())
