"""`python -m tiderace_shim <root> [--probe|--subinterp]` — the same dispatch as the entry file."""
import sys

from .modes import main

sys.exit(main())
