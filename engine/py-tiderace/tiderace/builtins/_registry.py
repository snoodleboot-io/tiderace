"""The one list of builtin providers (TID-111).

A builtin is an ordinary `@tiderace.provides` resource that the shim registers globally, so every
suite has it without an import in its own conftest. There used to be three places to add one —
the decorator, a hand-kept `providers()` literal and `__all__` — and `pytester` (TID-105) had to
touch all three. Now `@builtin` is the decorator, and the other two are derived from what it saw.
"""
from __future__ import annotations

from typing import Callable

import tiderace

_PROVIDERS: list[Callable] = []


def builtin(_fn: Callable | None = None, **provides_kwargs):
    """`@tiderace.provides`, and registered as a builtin — in definition order, which is the
    order the shim registers them in. Keyword arguments are `provides`' own (`scope=`, `type=`)."""

    def deco(fn: Callable) -> Callable:
        fn = tiderace.provides(fn, **provides_kwargs)
        _PROVIDERS.append(fn)
        return fn

    return deco(_fn) if _fn is not None else deco


def providers() -> list[Callable]:
    """The builtin provider callables, for the shim to register globally (always-available)."""
    return list(_PROVIDERS)
