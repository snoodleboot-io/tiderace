"""`python -m tiderace_shim <root> [--probe|--subinterp]` — the same dispatch as the entry file."""
import sys

from ._shim import main

sys.exit(main())
