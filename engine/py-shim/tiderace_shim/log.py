"""The shim's one line to the user: stderr, flushed — stdout is the protocol (TID-103)."""
from __future__ import annotations

import sys


def warn(message: str) -> None:
    print(f"tiderace: {message}", file=sys.stderr, flush=True)
