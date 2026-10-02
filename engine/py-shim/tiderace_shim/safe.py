"""Attribute access that treats *any* exception as "absent" (TID-43).

Discovery probes every module-level value to see whether it is a fixture or a provider, and those
values are arbitrary objects. Plenty of them raise something other than `AttributeError` when
touched: flask's `request`, `g`, `session` and `current_app` are werkzeug `LocalProxy` objects that
raise `RuntimeError: Working outside of request context`; Django's `SimpleLazyObject` can raise
whatever its factory raises; a mock can have a side effect on attribute access.

Plain `hasattr` and `getattr(obj, name, default)` only swallow `AttributeError`, so any of those
escaped `_discover` and killed the shim before it was ready — which on flask made the default
configuration hang forever (see `WellspringPool::launch`). pytest's discovery goes through
`_pytest.compat.safe_getattr` for precisely this reason; this is the same contract.

`BaseException` is deliberately *not* caught: `KeyboardInterrupt` and `SystemExit` from a probe
mean the user or the interpreter wants out, and swallowing them would be its own bug.
"""
from __future__ import annotations

from typing import Any

MISSING = object()


def safe_getattr(obj: Any, name: str, default: Any = None) -> Any:
    try:
        return getattr(obj, name, default)
    except Exception:  # noqa: BLE001 — see the module docstring; this is the point
        return default


def safe_hasattr(obj: Any, name: str) -> bool:
    return safe_getattr(obj, name, MISSING) is not MISSING
